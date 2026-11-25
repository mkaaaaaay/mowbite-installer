//! Texts for the codes of the Tron module: (key, de, en).

pub const TEXTS: &[(&str, &str, &str)] = &[
    (
        "tron.usb_mode_unknown",
        "Die Tron meldet sich in einem USB-Modus, den der Installer nicht kennt. Er hat ihr nichts gesendet.",
        "The Tron reports a USB mode the installer does not know. It sent nothing to it.",
    ),
    (
        "tron.loader_not_started",
        "Der Loader ist auf der Tron nicht gestartet. Den Akku der Tron ab- und wieder anstecken, andere \
         USB-Werkzeuge schließen, die Tron neu in den USB-Modus bringen und den Installer erneut starten. Auf \
         die Tron wurde nichts geschrieben.",
        "The loader did not start on the Tron. Take the battery off and put it back, close other USB tools, bring \
         the Tron into USB mode again and start the installer again. Nothing was written to the Tron.",
    ),
    (
        "tron.usb_driver_declined",
        "Ohne die Erlaubnis von Windows richtet der Installer keinen USB-Treiber ein. Beim nächsten Start erneut \
         fragen lassen.",
        "Without the permission of Windows the installer sets up no USB driver. Start it again to be asked again.",
    ),
    (
        "tron.usb_driver_failed",
        "Der USB-Treiber für die Tron ließ sich nicht einrichten. Die Tron neu verbinden und den Installer erneut \
         starten.",
        "The USB driver for the Tron could not be set up. Connect the Tron again and start the installer again.",
    ),
    (
        "tron.usb_driver_missing",
        "Das Hilfsprogramm für den USB-Treiber fehlt. Den Installer vollständig aus dem ZIP-Archiv entpacken.",
        "The helper for the USB driver is missing. Unpack the whole ZIP archive of the installer.",
    ),
    (
        "tron.compare_device",
        "Die Sicherung stammt aus einem früheren Lauf. Der Installer vergleicht sie mit der Tron.",
        "The backup comes from an earlier run. The installer compares it with the Tron.",
    ),
    (
        "tron.device_changed",
        "Die Tron hat sich seit dieser Sicherung verändert (gestartet oder aktualisiert). Bitte mit einem neuen, \
         leeren Sicherungsordner neu beginnen. Die alte Sicherung nicht löschen, bis die neue fertig ist.",
        "The Tron changed since this backup (it started or was updated). Please start again with a new, empty \
         backup folder. Keep the old backup until the new one is done.",
    ),
    (
        "tron.no_vendor_system",
        "Auf der Tron läuft nicht das System des Herstellers. Der Installer sichert nur einen Werksstand; \
         für eine Tron mit MowBite OS den Ordner der ersten Sicherung nehmen.",
        "The Tron does not run the vendor system. The installer only backs up a factory state; for a Tron with \
         MowBite OS use the folder of the first backup.",
    ),
    (
        "tron.part_link",
        "Ein benötigter Teil des Herstellers enthält einen Verweis nach außerhalb. So kann er nicht übernommen werden.",
        "A required vendor part holds a link to outside. It cannot be taken over like this.",
    ),
    (
        "tron.optional_left_out",
        "Ein zusätzlicher Teil des Herstellers enthält einen Verweis nach außerhalb und bleibt weg.",
        "An extra vendor part holds a link to outside and is left out.",
    ),
    (
        "tron.work_image_kept",
        "Das Arbeitsabbild mit den WLAN-Schlüsseln ließ sich nicht löschen. Bitte den Arbeitsordner von Hand leeren.",
        "The work image with the Wi-Fi keys could not be removed. Please empty the work folder by hand.",
    ),
    (
        "tron.guide_battery",
        "Der Akku bleibt in der Tron. Die Tron steht nicht auf der Ladestation und ist ausgeschaltet.",
        "The battery stays in the Tron. The Tron is off and not on its charging station.",
    ),
    (
        "tron.guide_camera",
        "Die obere Kamera-Einheit abnehmen. Dahinter liegt eine USB-C-Buchse.",
        "Take off the upper camera unit. A USB-C socket sits behind it.",
    ),
    (
        "tron.guide_cable",
        "Ein USB-Kabel von dieser Buchse zum PC stecken.",
        "Connect a USB cable from this socket to the PC.",
    ),
    (
        "tron.guide_button",
        "Taste B gedrückt halten und die Tron einschalten. Die Taste loslassen, wenn der Installer die Tron sieht.",
        "Hold button B and switch the Tron on. Let go when the installer sees the Tron.",
    ),
    (
        "tron.no_serial",
        "Die Seriennummer der Tron ist nicht lesbar. Ohne sie kann der Installer die Sicherung nicht sicher zuordnen.",
        "The serial number of the Tron cannot be read. Without it the installer cannot match the backup safely.",
    ),
    (
        "tron.table_changed",
        "Die Partitionstabelle der Tron hat sich seit der Sicherung geändert.",
        "The partition table of the Tron changed since the backup.",
    ),
    (
        "tron.table_unreadable",
        "Die Partitionstabelle der Tron ist nicht lesbar.",
        "The partition table of the Tron cannot be read.",
    ),
    (
        "tron.table_unknown",
        "Die Partitionstabelle dieser Tron kennt der Installer nicht.",
        "The installer does not know the partition table of this Tron.",
    ),
    (
        "tron.boot_unreadable",
        "Der Startbereich (boot) der Tron ist nicht lesbar.",
        "The boot partition of the Tron cannot be read.",
    ),
    (
        "tron.boot_signed",
        "Der Start dieser Tron ist signiert. Ein eigenes System würde sie nicht starten.",
        "The boot of this Tron is signed. It would not start a different system.",
    ),
    (
        "tron.root_read_only",
        "Diese Tron startet ihr System schreibgeschützt.",
        "This Tron starts its system read-only.",
    ),
    (
        "tron.root_verified",
        "Diese Tron prüft ihr System beim Start (verity). Ein eigenes System würde sie nicht starten.",
        "This Tron verifies its system at boot (verity). It would not start a different system.",
    ),
    (
        "tron.root_elsewhere",
        "Die Tron startet ihr System nicht aus der Partition, die der Installer ersetzen würde.",
        "The Tron does not start its system from the partition the installer would replace.",
    ),
    (
        "tron.rootfs_unreadable",
        "Das System der Tron (rootfs) ist in der Sicherung nicht lesbar.",
        "The system of the Tron (rootfs) cannot be read from the backup.",
    ),
    (
        "tron.modules_missing",
        "Die Treiber für den Kernel dieser Tron fehlen in ihrem System.",
        "The drivers for the kernel of this Tron are missing from its system.",
    ),
    (
        "tron.modules_ignored",
        "Treiber für einen anderen Kernel liegen im System; sie werden nicht gebraucht.",
        "Drivers for another kernel are in the system; they are not needed.",
    ),
    (
        "tron.module_other_kernel",
        "Ein Treiber passt nicht zum Kernel dieser Tron.",
        "A driver does not match the kernel of this Tron.",
    ),
    (
        "tron.part_missing",
        "Ein Teil des Herstellersystems, ohne den MowBite OS nicht läuft, fehlt.",
        "A part of the vendor system that MowBite OS needs is missing.",
    ),
    (
        "tron.part_unwritten",
        "Ein Teil des Herstellersystems, ohne den MowBite OS nicht läuft, ist nicht sicher lesbar.",
        "A part of the vendor system that MowBite OS needs cannot be read reliably.",
    ),
    (
        "tron.optional_missing",
        "Ein Zusatzteil des Herstellersystems fehlt; es wird ohne ihn eingerichtet.",
        "An optional part of the vendor system is missing; setup goes on without it.",
    ),
    (
        "tron.optional_unwritten",
        "Ein Zusatzteil des Herstellersystems ist nicht sicher lesbar; es wird ohne ihn eingerichtet.",
        "An optional part of the vendor system cannot be read reliably; setup goes on without it.",
    ),
    (
        "tron.vendor_unknown",
        "Diesen Softwarestand des Herstellers kennt der Installer nicht. Er kann trotzdem weitermachen.",
        "The installer does not know this vendor software version. It can go on anyway.",
    ),
    (
        "tron.write",
        "MowBite OS wird auf die Tron geschrieben.",
        "Writing MowBite OS to the Tron.",
    ),
    (
        "tron.read_back",
        "Das Geschriebene wird zurückgelesen und verglichen.",
        "Reading back what was written and comparing it.",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCES: &[&str] = &[
        include_str!("back.rs"),
        include_str!("backup.rs"),
        include_str!("checks.rs"),
        include_str!("fit.rs"),
        include_str!("gpt.rs"),
        include_str!("identity.rs"),
        include_str!("image.rs"),
        include_str!("module.rs"),
        include_str!("contents.rs"),
        include_str!("port.rs"),
        include_str!("pack.rs"),
        include_str!("rootfs.rs"),
        include_str!("tables.rs"),
        include_str!("usb.rs"),
        include_str!("usb_driver.rs"),
    ];

    #[test]
    fn every_code_has_a_text_and_every_text_a_code() {
        let mut used: Vec<&str> = SOURCES
            .iter()
            .flat_map(|s| s.split('"').skip(1).step_by(2))
            .filter(|s| {
                s.strip_prefix("tron.").is_some_and(|r| {
                    !r.is_empty() && r.chars().all(|c| c.is_ascii_lowercase() || c == '_')
                })
            })
            .collect();
        used.sort();
        used.dedup();
        let mut have: Vec<&str> = TEXTS.iter().map(|t| t.0).collect();
        have.sort();
        let n = have.len();
        have.dedup();
        assert_eq!(n, have.len(), "a key twice");
        let missing: Vec<_> = used.iter().filter(|c| !have.contains(c)).collect();
        assert!(missing.is_empty(), "no text for {missing:?}");
        let unused: Vec<_> = have.iter().filter(|c| !used.contains(c)).collect();
        assert!(unused.is_empty(), "text without code {unused:?}");
        assert!(TEXTS.iter().all(|t| !t.1.is_empty() && !t.2.is_empty()));
    }
}
