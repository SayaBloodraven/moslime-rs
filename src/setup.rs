//! `moslime --setup` (Linux): finds your pucks, pairs and trusts them with
//! BlueZ, and writes their addresses into config_six.json.
//!
//! It drives the same `bluetoothctl` commands you would type by hand
//! (select / power on / scan on / pair / trust), so it behaves the same way.

#[cfg(not(target_os = "linux"))]
pub fn run(_rt: &tokio::runtime::Runtime) -> anyhow::Result<()> {
    anyhow::bail!("--setup only works on Linux (it drives BlueZ). On Windows, use the window's Scan MACs.")
}

#[cfg(target_os = "linux")]
pub use linux::run;

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::BTreeMap;
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    use anyhow::{bail, Context, Result};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::{Child, ChildStdin, Command};
    use tokio::runtime::Runtime;
    use tokio::sync::mpsc;

    use crate::config;

    /// Address prefixes used by Raspberry Pi's built-in Bluetooth.
    const PI_OUIS: [&str; 5] = ["D8:3A:DD", "DC:A6:32", "E4:5F:01", "2C:CF:67", "B8:27:EB"];
    const SCAN_SECONDS: u64 = 45;

    struct Adapter {
        name: String,
        address: String,
    }

    async fn list_adapters() -> Result<Vec<Adapter>> {
        let session = bluer::Session::new()
            .await
            .context("could not talk to BlueZ (is the bluetooth service running?)")?;
        let mut out = Vec::new();
        for name in session.adapter_names().await? {
            let a = session.adapter(&name)?;
            let address = a.address().await?.to_string().to_uppercase();
            // Make sure the radio is on (otherwise BlueZ answers "NotReady").
            let _ = a.set_powered(true).await;
            out.push(Adapter { name, address });
        }
        Ok(out)
    }

    async fn ask(prompt: &str) -> String {
        use std::io::Write;
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        let mut r = BufReader::new(tokio::io::stdin());
        let _ = r.read_line(&mut line).await;
        line.trim().to_string()
    }

    /// Removes colour codes and prompt noise from a line of bluetoothctl output.
    fn clean(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if n.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
                continue;
            }
            if c == '\r' || c == '\x01' || c == '\x02' {
                continue;
            }
            out.push(c);
        }
        out.trim().to_string()
    }

    struct Ctl {
        _child: Child,
        stdin: ChildStdin,
        rx: mpsc::UnboundedReceiver<String>,
    }

    impl Ctl {
        async fn start() -> Result<Self> {
            let mut child = Command::new("bluetoothctl")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .context("could not run bluetoothctl (is the bluez package installed?)")?;
            let stdin = child.stdin.take().context("no stdin")?;
            let stdout = child.stdout.take().context("no stdout")?;
            let (tx, rx) = mpsc::unbounded_channel();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let c = clean(&line);
                    if !c.is_empty() && tx.send(c).is_err() {
                        break;
                    }
                }
            });
            Ok(Self {
                _child: child,
                stdin,
                rx,
            })
        }

        async fn send(&mut self, cmd: &str) -> Result<()> {
            self.stdin.write_all(format!("{cmd}\n").as_bytes()).await?;
            self.stdin.flush().await?;
            Ok(())
        }

        async fn next_line(&mut self, until: Instant) -> Option<String> {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            tokio::time::timeout(left, self.rx.recv()).await.ok().flatten()
        }
    }

    /// Picks "AA:BB:CC:DD:EE:FF" and an optional name out of a bluetoothctl line
    /// such as "[NEW] Device 3C:38:F4:.. QM-SS1 170E9",
    /// "[CHG] Device 3C:38:F4:.. Name: QM-SS1 170E9" or "Device 3C:38:F4:.. QM-SS1 170E9".
    fn parse_device(line: &str) -> Option<(String, Option<String>)> {
        let idx = line.find("Device ")?;
        let rest = &line[idx + "Device ".len()..];
        if rest.len() < 17 || !rest.is_char_boundary(17) {
            return None;
        }
        let mac = rest[..17].to_uppercase();
        let ok = mac.split(':').count() == 6 && mac.split(':').all(|p| p.len() == 2);
        if !ok {
            return None;
        }
        let tail = rest[17..].trim();
        let name = if line.contains("[CHG]") {
            tail.strip_prefix("Name:").map(|n| n.trim().to_string())
        } else if line.contains("[DEL]") {
            None
        } else if tail.is_empty() {
            None
        } else {
            Some(tail.to_string())
        };
        // Unnamed devices are shown with their address (dashes) as the name.
        let name = name.filter(|n| !n.contains('-') || n.starts_with("QM-SS1"));
        Some((mac, name))
    }

    fn is_puck(mac: &str, name: &Option<String>) -> bool {
        mac.starts_with("3C:38:F4")
            || name.as_deref().map(|n| n.starts_with("QM-SS1")).unwrap_or(false)
    }

    /// The printed ID: "QM-SS1 170E9" -> "170E9".
    fn short_id(name: &str) -> String {
        name.strip_prefix("QM-SS1").unwrap_or(name).trim().to_string()
    }

    /// Pairs and trusts one puck, retrying once, then disconnects it so the
    /// idle link doesn't get in the way of the next puck.
    async fn pair_one(ctl: &mut Ctl, mac: &str) -> std::result::Result<(), String> {
        let mut result = pair_once(ctl, mac).await;
        if result.is_err() {
            tokio::time::sleep(Duration::from_secs(3)).await;
            result = pair_once(ctl, mac).await;
        }
        if result.is_ok() {
            let _ = ctl.send(&format!("disconnect {mac}")).await;
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        result
    }

    async fn pair_once(ctl: &mut Ctl, mac: &str) -> std::result::Result<(), String> {
        ctl.send(&format!("pair {mac}")).await.map_err(|e| e.to_string())?;
        let until = Instant::now() + Duration::from_secs(40);
        let mut done = false;
        while let Some(line) = ctl.next_line(until).await {
            if line.contains("AlreadyExists") || line.contains("Already Exists") {
                done = true;
                break;
            }
            if line.contains("Pairing successful") {
                done = true;
                break;
            }
            if line.contains("Failed to pair") {
                return Err(line);
            }
            if line.contains("Confirm passkey") || line.contains("Request confirmation") {
                let _ = ctl.send("yes").await;
            }
        }
        if !done {
            return Err("timed out waiting for the puck".to_string());
        }

        ctl.send(&format!("trust {mac}")).await.map_err(|e| e.to_string())?;
        let until = Instant::now() + Duration::from_secs(10);
        while let Some(line) = ctl.next_line(until).await {
            if line.contains("trust succeeded") {
                return Ok(());
            }
            if line.contains("Failed to") {
                return Err(line);
            }
        }
        Err("could not mark the puck as trusted".to_string())
    }

    pub fn run(rt: &Runtime) -> Result<()> {
        rt.block_on(run_async())
    }

    async fn run_async() -> Result<()> {
        println!("moslime setup: find and pair your mocopi pucks\n");

        // 1. Choose the Bluetooth adapter.
        let adapters = list_adapters().await?;
        if adapters.is_empty() {
            bail!(
                "no Bluetooth adapter found. Plug in the dongle (try: sudo apt install firmware-realtek) and run this again."
            );
        }
        let mut chosen = 0;
        if adapters.len() > 1 {
            // Prefer a USB dongle over the Pi's built-in radio.
            chosen = adapters
                .iter()
                .position(|a| !PI_OUIS.iter().any(|p| a.address.starts_with(p)))
                .unwrap_or(0);
            println!("Bluetooth adapters found:");
            for (i, a) in adapters.iter().enumerate() {
                let note = if PI_OUIS.iter().any(|p| a.address.starts_with(p)) {
                    " (Raspberry Pi built-in)"
                } else {
                    ""
                };
                println!("  {}) {}  {}{}", i + 1, a.name, a.address, note);
            }
            let ans = ask(&format!("Which one should the pucks use? [{}] ", chosen + 1)).await;
            if let Ok(n) = ans.parse::<usize>() {
                if n >= 1 && n <= adapters.len() {
                    chosen = n - 1;
                }
            }
        }
        let adapter = &adapters[chosen];
        println!("Using {} ({})\n", adapter.name, adapter.address);

        let want = ask("How many pucks do you have? [6] ").await.parse::<usize>().unwrap_or(6).clamp(1, config::TRACKERS);
        println!("\nTurn all pucks ON (double blue blink) and keep them away from your phone.");
        println!("Scanning for up to {SCAN_SECONDS} seconds...\n");

        // 2. Scan.
        let mut ctl = Ctl::start().await?;
        ctl.send(&format!("select {}", adapter.address)).await?;
        ctl.send("power on").await?;
        ctl.send("devices").await?;
        ctl.send("scan on").await?;

        let mut found: BTreeMap<String, String> = BTreeMap::new(); // mac -> name
        let until = Instant::now() + Duration::from_secs(SCAN_SECONDS);
        let mut last_count = 0;
        while let Some(line) = ctl.next_line(until).await {
            if let Some((mac, name)) = parse_device(&line) {
                if is_puck(&mac, &name) {
                    let e = found.entry(mac).or_default();
                    if let Some(n) = name {
                        *e = n;
                    }
                }
            }
            if found.len() != last_count {
                last_count = found.len();
                println!("  found {} of {}", found.len(), want);
            }
            if found.len() >= want && found.values().all(|n| !n.is_empty()) {
                break;
            }
        }

        if found.is_empty() {
            let _ = ctl.send("scan off").await;
            let _ = ctl.send("quit").await;
            bail!("no pucks found. Turn them on, keep them close, and run --setup again.");
        }

        println!("\nPucks found:");
        for (mac, name) in &found {
            let id = if name.is_empty() { "(name not seen)".to_string() } else { short_id(name) };
            println!("  {mac}   ID {id}");
        }
        if found.len() < want {
            println!(
                "\nOnly {} of {} found. Pairing these now; switch the missing ones off and on and run --setup again afterwards.",
                found.len(),
                want
            );
        }

        // 3. Pair and trust each one (the scan stays on, like doing it by hand).
        println!("\nPairing...");
        let mut paired: Vec<String> = Vec::new();
        for mac in found.keys() {
            match pair_one(&mut ctl, mac).await {
                Ok(()) => {
                    println!("  {mac}: paired and trusted");
                    paired.push(mac.clone());
                }
                Err(e) => println!("  {mac}: FAILED ({e})"),
            }
        }
        let _ = ctl.send("scan off").await;
        let _ = ctl.send("quit").await;

        if paired.is_empty() {
            bail!("no puck could be paired. Check the dongle and turn the pucks off and on, then try again.");
        }

        // 4. Save to the config: keep the slot order already in the file,
        //    fill free slots with the new pucks (sorted by printed ID).
        let settings = config::load();
        let existing: Vec<String> = settings
            .configured
            .iter()
            .map(|s| s.trim().to_uppercase())
            .collect();
        let mut slots: Vec<Option<String>> = vec![None; config::TRACKERS];
        for (i, e) in existing.iter().enumerate().take(config::TRACKERS) {
            if paired.contains(e) {
                slots[i] = Some(e.clone());
            }
        }
        let mut remaining: Vec<String> = paired
            .iter()
            .filter(|m| !slots.iter().any(|s| s.as_deref() == Some(m.as_str())))
            .cloned()
            .collect();
        remaining.sort_by_key(|m| short_id(found.get(m).map(|s| s.as_str()).unwrap_or("")));
        let mut remaining = remaining.into_iter();
        for slot in slots.iter_mut() {
            if slot.is_none() {
                *slot = remaining.next();
            }
        }
        let macs: Vec<String> = slots
            .iter()
            .enumerate()
            .map(|(i, s)| match s {
                Some(m) => m.clone(),
                // Keep whatever was there (a puck not paired this time).
                None => existing.get(i).cloned().unwrap_or_default(),
            })
            .collect();
        config::save(&macs, settings.compat)?;
        let ids: BTreeMap<String, String> = found
            .iter()
            .filter(|(_, n)| !n.is_empty())
            .map(|(m, n)| (m.clone(), short_id(n)))
            .collect();
        config::save_ids(&ids)?;

        // Remember which adapter holds the pairings (only matters with several).
        if adapters.len() > 1 {
            config::save_adapter(&adapter.name)?;
        }

        if settings.server == config::DEFAULT_SERVER {
            let ip = ask("\nIP address of the PC running SlimeVR (e.g. 192.168.1.50), or Enter to skip: ").await;
            if !ip.is_empty() {
                match config::parse_server(&ip) {
                    Ok(a) => config::save_server(&a.to_string())?,
                    Err(e) => println!("Not saved: {e:#}"),
                }
            }
        }

        println!("\nSaved to {}:", config::CONFIG_FILE);
        for (i, m) in macs.iter().enumerate() {
            if m.is_empty() {
                continue;
            }
            let id = found.get(m).map(|n| short_id(n)).unwrap_or_default();
            println!("  tracker #{}: {}   {}", i + 1, m, if id.is_empty() { String::new() } else { format!("ID {id}") });
        }
        println!("\nAll set. Start the bridge with:  ./moslime --headless");
        Ok(())
    }
}
