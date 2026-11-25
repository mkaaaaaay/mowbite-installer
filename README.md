# MowBite Installer

<img src="docs/logo.svg" alt="MowBite logo" width="96" align="right">

Puts MowBite OS on an Airseekers Tron from a Windows or Linux PC over a USB cable,
and takes it back to the vendor system whenever you want. Before it writes anything it reads a full backup of the
mower's flash, and that backup is what the way back uses.

The MowBite Installer is a community project and not affiliated with Airseekers.

**Not ready for use yet.** The installer runs against a file copy of a Tron's flash, but not on a mower yet: opening a
real Tron over USB stops on purpose until its USB modes are measured, and there are no release files to install
yet. Follow the progress on the [MowBite Discord](https://discord.gg/VpPFRx9Ce4).

- [What it does](#what-it-does)
- [What you need](#what-you-need)
- [Use it](#use-it)
- [Safety and your data](#safety-and-your-data)
- [Build](#build)
- [License](#license)

## What it does

1. **Checks the PC**: room for the backup and the work files.
2. **Finds the Tron** in its USB mode. On Windows it sets up the WinUSB driver once, if the Tron has none yet.
3. **Backs it up**: the whole flash once, the system and every boot area a second time, compared by sha256. A backup
   that was started is resumed, never replaced.
4. **Checks the mower** before anything is written: partition table, vendor version, kernel and the parts MowBite OS
   needs from the vendor system. An unknown vendor version gets a clear warning first.
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

- A PC with Windows 10 or 11 (x64) or Linux (x86_64).
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

Download the installer for your PC from the releases of this repo, and the release folder with the signed system,
bundles and tables. Then, in a terminal:

```
mowbite-installer install <backup folder> <release folder> [--work <folder>]
mowbite-installer back <backup folder> <release folder>
```

The installer asks for your Wi-Fi and then shows how to connect the Tron:

1. Battery in, Tron off and not on its charging station.
2. Take off the upper camera unit. A USB-C socket sits behind it.
3. Connect the cable from this socket to the PC.
4. Hold button B and switch the Tron on. Let go when the installer sees the Tron.

From there it runs by itself and keeps the PC awake. Plan for one to two hours. If it stops, start it again with the
same folders and it goes on where it was.

On Windows the WinUSB step shows a Windows prompt for permission. It uses
[libwdi](https://github.com/pbatard/libwdi) like Zadig does: `mowbite-usb-driver.exe` and `libwdi.dll` have to sit
next to `mowbite-installer.exe`.

## Safety and your data

- **Keep the backup to yourself.** It holds the whole flash of your mower with its keys and your Wi-Fi. Do not share
  it, and keep it until you are sure you will not go back.
- The installer writes only into the system partition, and only after the backup and all checks passed. A check that
  fails ends the run before the first write.
- It never sends a drive or blade command. The new system starts with the hardware off until the setup in the app.
- Everything it installs is checked against keys built into the installer. Unsigned or changed files are refused.
- Nothing of the vendor is downloaded or shared: the parts the new system needs come from your own backup.
- The MQTT broker on the mower has no login. Keep the mower in a network you trust and never open its ports to the
  internet.

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
