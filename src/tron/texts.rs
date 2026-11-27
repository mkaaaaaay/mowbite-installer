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
        "tron.usb_access_declined",
        "Ohne dein Passwort legt der Installer die USB-Regel nicht an. Beim nächsten Start erneut fragen lassen.",
        "Without your password the installer sets up no USB rule. Start it again to be asked again.",
    ),
    (
        "tron.usb_access_failed",
        "Der Installer erreicht die Tron über USB noch nicht. Das USB-Kabel ab- und wieder anstecken und den \
         Installer erneut starten. Hilft das nicht: direkt an diesem Rechner angemeldet sein, nicht über das Netz.",
        "The installer cannot reach the Tron over USB yet. Unplug the USB cable, plug it in again and start the \
         installer again. If that does not help: be logged in at this computer itself, not over the network.",
    ),
    (
        "tron.usb_access_manual",
        "Für den USB-Zugriff auf die Tron fehlt eine Regel, und pkexec ist nicht da. Bitte diesen Befehl in einem \
         Terminal ausführen und den Installer erneut starten:",
        "A rule for USB access to the Tron is missing and pkexec is not there. Please run this command in a terminal \
         and start the installer again:",
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
        "tron.firmware_old",
        "Diese Tron hat einen älteren Softwarestand des Herstellers, mit dem MowBite OS nicht fahren würde. Zuerst in \
         der Airseekers-App das neueste Update einspielen, die Tron danach 10 Minuten eingeschaltet lassen (sie \
         aktualisiert dabei ihre Platinen) und den Installer erneut starten. Auf die Tron wurde nichts geschrieben.",
        "This Tron has an older vendor software version MowBite OS would not drive with. First install the latest \
         update in the Airseekers app, leave the Tron switched on for 10 minutes afterwards (it updates its boards \
         meanwhile) and start the installer again. Nothing was written to the Tron.",
    ),
    (
        "tron.firmware_new",
        "Diese Tron hat neuere Platinen-Firmware, als MowBite OS bisher kennt. Fahren und Messer bleiben gesperrt, \
         bis die Sicherheitsmessung in der App bestanden ist; sie dauert etwa 15 Minuten.",
        "This Tron has newer board firmware than MowBite OS knows so far. Driving and the blade stay locked until \
         the safety measurement in the app has passed; it takes about 15 minutes.",
    ),
    (
        "tron.firmware_unknown",
        "Der Installer kann nicht erkennen, welche Platinen-Firmware diese Tron bekommt. Ist sie nicht freigegeben, \
         bleiben Fahren und Messer gesperrt, bis die Sicherheitsmessung in der App bestanden ist.",
        "The installer cannot tell which board firmware this Tron gets. If it is not cleared, driving and the blade \
         stay locked until the safety measurement in the app has passed.",
    ),
    (
        "tron.report",
        "Diesen Stand hat uns noch niemand gemeldet. Schick uns bitte die Angaben in der Klammer, dann nehmen wir \
         ihn auf: https://github.com/mkaaaaaay/mowbite-installer/issues",
        "Nobody has reported this stand to us yet. Please send us what is in the brackets, then we add it: \
         https://github.com/mkaaaaaay/mowbite-installer/issues",
    ),
    (
        "tron.vendor_unknown",
        "Diesen Softwarestand des Herstellers kennt der Installer nicht. Er kann trotzdem weitermachen.",
        "The installer does not know this vendor software version. It can go on anyway.",
    ),
    (
        "tron.before_install",
        "Vorher: Hersteller-Software 1.3.32 oder 1.3.33 auf der Tron (der Installer prüft das), Akku mindestens halb \
         voll, etwa 42 GB frei im Sicherungsordner. Mit der Sicherung kommt das Hersteller-System jederzeit zurück. \
         Bricht etwas ab, den Installer mit demselben Ordner neu starten.",
        "Before you start: vendor software 1.3.32 or 1.3.33 on the Tron (the installer checks this), battery at least \
         half full, about 42 GB free in the backup folder. The backup brings the vendor system back at any time. \
         Should something break off, start the installer again with the same folder.",
    ),
    (
        "tron.before_check",
        "Vorher: Akku mindestens halb voll, etwa 25 GB frei im Sicherungsordner. Am Ende kannst du gleich \
         installieren. Später geht es mit derselben Sicherung nur weiter, wenn die Tron dazwischen nicht startet.",
        "Before you start: battery at least half full, about 25 GB free in the backup folder. At the end you can \
         install right away. Later it goes on from the same backup only if the Tron does not start in between.",
    ),
    (
        "tron.before_back",
        "Vorher: den Sicherungsordner dieser Tron wählen (der Installer prüft, ob er passt), Akku mindestens halb \
         voll. Zurückschreiben ist bisher nur unter Linux erprobt.",
        "Before you start: choose the backup folder of this Tron (the installer checks that it fits), battery at \
         least half full. Writing back has been tried on Linux only so far.",
    ),
    (
        "tron.wifi_name_bad",
        "Der WLAN-Name hat 1 bis 32 Zeichen (Umlaute zählen doppelt).",
        "The Wi-Fi name has 1 to 32 characters (accented letters count twice).",
    ),
    (
        "tron.wifi_key_bad",
        "Der WLAN-Schlüssel hat 8 bis 63 Zeichen, ohne Umlaute und andere Zeichen außerhalb von ASCII. Offene WLANs \
         und reines WPA3 kann der Installer nicht eintragen: am Router WPA2 oder WPA2+WPA3 einstellen.",
        "The Wi-Fi key has 8 to 63 characters, without accented letters and other characters outside ASCII. The \
         installer cannot set up open networks and WPA3-only ones: set the router to WPA2 or WPA2+WPA3.",
    ),
    (
        "tron.after_check",
        "Bis zum Installieren die Tron nicht einschalten und nicht auf die Ladestation stellen, sonst passt die \
         Sicherung nicht mehr und der Installer braucht einen neuen Ordner. Jetzt das USB-Kabel abziehen und den Akku \
         herausnehmen, damit die Tron aus ist. Die Kamera-Einheit wieder aufsetzen und festschrauben. Zum \
         Installieren den Akku einsetzen und weiter wie beim ersten Mal (Taste beim Einschalten halten).",
        "Until you install, don't switch the Tron on and don't put it on its charging station, or the backup no \
         longer matches and the installer needs a new folder. Now unplug the USB cable and take the battery out so \
         the Tron is off. Put the camera unit back on and screw it tight. To install, put the battery back and go on \
         as the first time (hold the button while switching on).",
    ),
    (
        "tron.after",
        "Zum Schluss: das USB-Kabel abziehen, die Kamera-Einheit wieder aufsetzen und festschrauben, damit die Tron \
         dicht bleibt, und die Tron auf ihre Ladestation stellen.",
        "To finish: unplug the USB cable, put the camera unit back on and screw it tight so the Tron stays sealed, \
         then place the Tron on its charging station.",
    ),
    (
        "tron.setup_wifi_later",
        "Findet die Tron später kein WLAN mehr: die Taste Ein/OK oben fünfmal kurz hintereinander drücken (innerhalb \
         von 5 Sekunden). Ein steigender Ton heißt, das WLAN der Tron ist für 30 Minuten offen. Die Taste nicht \
         gedrückt halten, das schaltet die Tron aus.",
        "If the Tron can't find a Wi-Fi later: press the On/OK button on top five times quickly (within 5 seconds). \
         A rising tone means the Tron's own Wi-Fi is open for 30 minutes. Don't hold the button, that switches the \
         Tron off.",
    ),
    (
        "tron.backup",
        "Der Installer sichert den ganzen Speicher der Tron, die Bereiche zum Starten und den Systembereich doppelt. Das dauert etwa 30 bis \
         55 Minuten; die Tron bleibt dabei angeschlossen und eingeschaltet. Auf die Tron wird nichts geschrieben.",
        "The installer saves the whole flash of the Tron, the areas it starts from and its system area twice. This takes about 30 to 55 \
         minutes; the Tron stays connected and switched on. Nothing is written to the Tron.",
    ),
    (
        "tron.check_system",
        "Der Installer prüft das gesicherte System der Tron. Das dauert etwa 10 Minuten, ohne Fortschrittsbalken.",
        "The installer checks the saved system of the Tron. This takes about 10 minutes, without a progress bar.",
    ),
    (
        "tron.check_backup",
        "Die Sicherung wird geprüft, bevor sie zurückgeschrieben wird.",
        "Checking the backup before writing it back.",
    ),
    (
        "tron.write",
        "MowBite OS wird auf die Tron geschrieben. Danach liest der Installer alles zur Prüfung zurück; der Balken \
         zählt beides.",
        "Writing MowBite OS to the Tron. Then the installer reads it all back to check it; the bar counts both.",
    ),
    (
        "tron.write_back",
        "Das Hersteller-System wird zurück auf die Tron geschrieben. Danach liest der Installer alles zur Prüfung \
         zurück; der Balken zählt beides.",
        "Writing the vendor system back to the Tron. Then the installer reads it all back to check it; the bar \
         counts both.",
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
