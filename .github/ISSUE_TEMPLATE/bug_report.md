---
name: Problem report
about: Something doesn't work, or works differently on your machine
labels: bug
---

**What happened, and what did you expect?**


**Your setup**
- moslime-rs version (release name, e.g. v0.2.0):
- Computer and system (e.g. Raspberry Pi 4B with Raspberry Pi OS 64-bit, or Windows 11 PC):
- Bluetooth adapter (brand and model, or "built-in"):
- Number of pucks:

**Log**
On Linux, run this right after the problem and paste the result:

    journalctl -u moslime --since "-15min"

(If you start it by hand instead of as a service, copy what it printed.)
On Windows, copy the text from the window's log, or a screenshot of the window.

**Anything else**
Battery levels, how far the pucks are from the adapter, whether it worked
before, and so on.
