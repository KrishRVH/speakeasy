use super::*;

const SHOW: &str = r"C:\Users\Ada Lovelace\AppData\Local\speakeasy\shows\20261117T093000Z";

fn mapped(conf: &str, windows: &str) -> Option<String> {
    Wsl::configured(conf).path(Path::new(windows))
}

#[test]
fn drives_mount_under_mnt_unless_wsl_conf_moves_or_disables_them() {
    let under = |root: &str| {
        Some(format!(
            "{root}c/Users/Ada Lovelace/AppData/Local/speakeasy/shows/20261117T093000Z"
        ))
    };
    for (conf, expected) in [
        ("", under("/mnt/")),
        (
            "[boot]\nsystemd=true\n[user]\ndefault=ada\n",
            under("/mnt/"),
        ),
        ("[automount]\nroot = /windir\n", under("/windir/")),
        ("[automount]\r\nroot = /windir/\r\n", under("/windir/")),
        ("[automount]\nroot = /\n", under("/")),
        ("[automount]\nroot =\n", under("/")),
        ("[AutoMount]\nROOT = /windir\n", under("/windir/")),
        (
            "  [automount] # drives\n\troot=/windir # moved\n",
            under("/windir/"),
        ),
        (
            "[automount]\nroot = \"/my drives/\"\n",
            under("/my drives/"),
        ),
        ("[automount]\nroot = \"/a#b\"\n", under("/a#b/")),
        ("[automount]\nroot = /a#b\n", under("/a/")),
        (
            "[automount]\nroot = /first\nroot = /second\n",
            under("/first/"),
        ),
        ("[automount]\n# root = /commented\n", under("/mnt/")),
        ("[boot]\nroot = /elsewhere\n", under("/mnt/")),
        (
            "[automount]\nroot = \"/unterminated\nroot = /next\n",
            under("/next/"),
        ),
        (
            "[automount]\nroot = /bad\\escape\nroot = /next\n",
            under("/next/"),
        ),
        (
            "[automount]\n[auto mount]\nroot = /orphan\n",
            under("/mnt/"),
        ),
        ("[automount]\nroot = relative\n", None),
        ("[automount]\nroot = \"/new\\nline\"\n", None),
        ("[automount]\nenabled = false\n", None),
        ("[automount]\nenabled = FALSE\nroot = /windir\n", None),
        ("[automount]\nenabled = 0\n", None),
        ("[automount]\nenabled = no\n", under("/mnt/")),
        (
            "[automount]\nenabled = true\nenabled = false\n",
            under("/mnt/"),
        ),
    ] {
        assert_eq!(mapped(conf, SHOW), expected, "{conf:?}");
    }
}

#[test]
fn only_a_folder_on_a_lettered_drive_has_a_wsl_path() {
    for (windows, expected) in [
        (r"\\?\C:\Users\ada", Some("/mnt/c/Users/ada")),
        ("D:/Shows/one", Some("/mnt/d/Shows/one")),
        (r"E:\Shows\\one\", Some("/mnt/e/Shows/one")),
        (r"C:\", Some("/mnt/c")),
        ("C:", Some("/mnt/c")),
        (r"\\server\share\shows", None),
        (r"\\?\UNC\server\share\shows", None),
        (r"shows\one", None),
        ("C:shows", None),
        (r"1:\shows", None),
        (r"É:\shows", None),
    ] {
        assert_eq!(mapped("", windows).as_deref(), expected, "{windows:?}");
    }
}

#[test]
fn interop_off_keeps_the_folder_but_not_the_command() {
    let exe = Path::new(r"C:\Program Files\Speakeasy\speakeasy-cli.exe");
    let mounted = Some("/mnt/c/Program Files/Speakeasy/speakeasy-cli.exe");
    let off = Wsl::configured("[interop]\nenabled = false\n");
    assert_eq!(off.path(exe).as_deref(), mounted);
    assert_eq!(off.command(exe), None);
    let on = Wsl::configured("[interop]\nenabled = true\n");
    assert_eq!(on.command(exe).as_deref(), mounted);
}
