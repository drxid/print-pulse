# Print Pulse

A small Windows tray app that keeps an inkjet printer healthy. Inkjets dry out when they sit idle — ink thickens in the nozzles and prints come out streaky — so Print Pulse sends a short maintenance print on a schedule, and keeps an eye on ink, the maintenance box and the print queue.

<p align="center">
  <img src="reference/app-icon.svg" width="96" alt="Print Pulse icon" />
</p>

## Features

- **Scheduled maintenance prints** — every 1/3/5/7 days at a chosen time; a run missed while the PC was off happens on the next start.
- **Nozzle check** — the printer's own pattern, sent exactly as the Epson driver sends it: fires every nozzle with very little ink. Or a **colour test** page through the regular driver.
- **Head cleaning** on demand, behind a confirmation (it uses a lot of ink).
- **Counts regular printing** — anything you print pushes the schedule back.
- **Quiet hours and days** — no prints at night or on chosen weekdays; the run moves to the next allowed time.
- **Ink levels, page counter and maintenance box** over SNMP, with notifications when ink runs low or the box is nearly full.
- **Printer status** — wakes a sleeping network printer before printing, retries later if it's off, and tells you when a VPN is blocking the local network.
- **Stuck queue** detection with one‑click clearing.
- **System tray** — status in the menu, a green dot on the icon when active, click to open.
- **Autostart** at login, optionally **start minimized** to the tray.
- **Auto‑updates** — checks GitHub Releases and updates itself.
- **12 languages**, automatic from your system locale, with light/dark theme.

Built for and tested with an **EPSON L8180** on the network; other Epson inkjets that speak the same remote commands should work too.

## Tech stack

- [Tauri 2](https://v2.tauri.app/) (Rust) + [Svelte 5](https://svelte.dev/) + Vite
- Printing through the Windows spooler (winspool / GDI), status over SNMP

## License

MIT © [drxid](https://drxid.ru/)
