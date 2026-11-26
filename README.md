# MowBite OS installer for the Airseekers Tron

<img src="docs/logo.svg" alt="MowBite logo" width="96" align="right">

Puts MowBite OS on an Airseekers Tron from a Windows or Linux PC over a USB cable,
and takes it back to the vendor system whenever you want. Before it writes anything it reads a full backup of the
mower's flash, and that backup is what the way back uses.

The MowBite Installer is a community project and not affiliated with Airseekers.

**Test version.** The installer runs on a real Tron now, and we are testing it on our own. Use it only if you are
fine with a test: keep the backup it makes, the way back to the vendor system uses it. Follow the progress on the
[MowBite Discord](https://discord.gg/VpPFRx9Ce4).

![The MowBite app: dashboard, map, recording and schedule](https://raw.githubusercontent.com/mkaaaaaay/mowbite/main/docs/screens.webp)

After the installation the mower runs the [MowBite app](https://github.com/mkaaaaaay/mowbite): open it in the
browser on a phone or computer, or use the Android app.

- [What it does](#what-it-does)
- [What you need](#what-you-need)
- [Use it](#use-it)
- [Safety and your data](#safety-and-your-data)
- [The MowBite project](#the-mowbite-project)
- [Build](#build)
- [License](#license)

## What it does

1. **Gets the release**: downloads the newest signed release from
   [mowbite-releases](https://github.com/mkaaaaaay/mowbite-releases) into the backup folder (about 220 MB), unless
   you give it a release folder. Then checks the PC: room for the backup and the work files.
2. **Finds the Tron** in its USB mode. On Windows it sets up the WinUSB driver once, if the Tron has none yet.
3. **Backs it up**: the whole flash once, the system and every boot area a second time, compared by sha256. A backup
   that was started is resumed, never replaced.
4. **Checks the mower** before anything is written: partition table, vendor version, kernel, the parts MowBite OS
   needs from the vendor system and the firmware the vendor system puts on the boards. An unknown vendor version or
   board firmware gets a clear warning first; firmware older than MowBite OS knows stops the run, update the vendor
   system in the Airseekers app first.
5. **Writes MowBite OS** into the system partition only, reads it back and restarts the mower when both match. The
   boot area is never written.
6. **First start**: the mower joins your Wi-Fi and installs the OS, the planner and the app from what the installer
   gave it, without the internet. The installer finds it in your network and confirms the start. If the Wi-Fi does not
   work, the mower opens a setup Wi-Fi of its own; its name and key are on a sheet in the backup folder.
7. **Hands over to the app**: map, RTK and the safety measurement follow in the setup of the MowBite app. Until then
   the mower only takes remote control and the blade stays locked.

**Back to the vendor system**: `back` writes the system partition from your backup, after checking that the backup
belongs to this mower.

## What you need

- A PC with Windows 10 or 11 (x64) or Linux (x86_64), with internet for the download.
- The Tron on vendor software 1.3.32 or 1.3.33. The installer checks it before writing.
- Free space: about 25 GB for the backup and 16 GB for work files. Both can be on an external disk.
- A USB-C data cable.
- The Tron with its battery at least half full, off its charging station.
- The name and key of your Wi-Fi.
- On Linux, access to the Tron on USB without root, for example with this udev rule in
  `/etc/udev/rules.d/70-mowbite-tron.rules`:

  ```
  SUBSYSTEM=="usb", ATTR{idVendor}=="2207", ATTR{idProduct}=="350b", MODE="0660", TAG+="uaccess"
  ```

## Use it

Download the installer for your PC from the releases of this repo. Then, in a terminal:

```
mowbite-installer check <backup folder>
mowbite-installer install <backup folder> [--work <folder>]
mowbite-installer back <backup folder>
```

`check` is a safe first step: it makes the backup and runs every check, then ends without writing anything to the
mower. `install` with the same backup folder goes on from that backup. Each command downloads the release files into
`<backup folder>/release`; to use files you already have, give their folder after the backup folder.

The installer asks for your Wi-Fi (not on `check`) and then shows how to connect the Tron:

1. Battery in, Tron off and not on its charging station.
2. Take off the upper camera unit. A USB-C socket sits behind it.
3. Connect the cable from this socket to the PC.
4. Hold button B and switch the Tron on. Let go when the installer sees the Tron.

From there it runs by itself and keeps the PC awake. The backup alone takes about 30 to 55 minutes; plan for one to
two hours in all. If it stops, start it again with the same folders and it goes on where it was.

On Windows the WinUSB step shows a Windows prompt for permission. It uses
[libwdi](https://github.com/pbatard/libwdi) like Zadig does: `mowbite-usb-driver.exe` and `libwdi.dll` have to sit
next to `mowbite-installer.exe`.

## Safety and your data

What can go wrong: the installer never writes the areas the Tron starts from, so the USB mode it uses stays
reachable. The backup holds the start areas and the system area twice. Writing the vendor system back (`back`) has
been tested on Linux. The worst case is a system area written only in part, for example when the cable comes off
while writing: start the installer again with the same backup folder, it offers to write again or to go back. Should
that fail on Windows, run `back` from a Linux PC.

- **Keep the backup to yourself.** It holds the whole flash of your mower with its keys and your Wi-Fi. Do not share
  it, and keep it until you are sure you will not go back.
- The installer writes only into the system partition, and only after the backup and all checks passed. A check that
  fails ends the run before the first write.
- It never sends a drive or blade command. The new system starts with the hardware off until the setup in the app.
- Everything it installs is checked against keys built into the installer. Unsigned or changed files are refused.
- Nothing of the vendor is downloaded or shared: the parts the new system needs come from your own backup.
- The MQTT broker on the mower has no login. Keep the mower in a network you trust and never open its ports to the
  internet.

## The MowBite project

- [MowBite](https://github.com/mkaaaaaay/mowbite): the app on the mower, with live dashboard, map editor, recording
  areas by driving, schedule and history. Also on
  [Google Play](https://play.google.com/store/apps/details?id=io.github.mkaaaaaay.mowbite).
- [MowBite Planner](https://github.com/mkaaaaaay/mowbite-planner): the mowing planner, it keeps the whole mower
  away from edges and obstacles, not just its middle.
- [MowBite for Home Assistant](https://github.com/mkaaaaaay/mowbite-ha): Home Assistant integration for OpenMower
  mowers, with the MowBite card and map.
- [MowBite Updater](https://github.com/mkaaaaaay/mowbite-updater): keeps OpenMower, MowBite and the planner up to
  date on OpenMower mowers.
- [mowbite-releases](https://github.com/mkaaaaaay/mowbite-releases): the signed release files of MowBite OS this
  installer puts on the mower.
- [MowBite Discord](https://discord.gg/VpPFRx9Ce4): questions, test reports and news.

## Build

Rust 1.85 or newer:

```
cargo build --release
cargo test
```

The Windows helper for the USB driver is built by the GitHub workflow in `.github/workflows/build.yaml` with MSYS2:
libwdi as a DLL of its own and `windows/usb-driver.c`.

## License

PolyForm Strict 1.0.0 with an exception for pull requests, see [LICENSE](LICENSE). Commercial use needs a separate
license. libwdi comes under the GNU LGPL 3, see `windows/THIRD-PARTY.txt`.
