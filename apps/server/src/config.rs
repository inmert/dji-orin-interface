//! The server's settings: `config.toml` (every key optional, see the file
//! at the repository root for the defaults and what each one means), with
//! the environment variables the scripts and README name as per-run
//! overrides. Loaded once at start; nothing else in the server reads the
//! environment.

use std::collections::BTreeMap;
use std::net::SocketAddrV4;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

use dji_commands::Limits;
use dji_common::logln;

/// The default file, next to the scripts (`CONFIG` overrides).
pub const DEFAULT_PATH: &str = "config.toml";

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub web: Web,
    pub secrets: Secrets,
    pub ports: Ports,
    pub motors: Motors,
    pub gimbal: Gimbal,
    pub video: Video,
    pub speech: Speech,
    pub understanding: Understanding,
    pub limits: LimitsCfg,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Web {
    pub http: String,
    /// The TLS listener's address, or `off`.
    pub https: String,
    pub root: PathBuf,
    pub cert_dir: PathBuf,
    /// Further names the portal is reached by (the Orin's own hostname and
    /// addresses are always accepted).
    pub hosts: Vec<String>,
}

impl Default for Web {
    fn default() -> Self {
        Self {
            http: "0.0.0.0:8080".into(),
            https: "0.0.0.0:8443".into(),
            root: "web/dist".into(),
            cert_dir: "certs".into(),
            hosts: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Secrets {
    pub dir: PathBuf,
}

impl Default for Secrets {
    fn default() -> Self {
        Self {
            dir: "secrets".into(),
        }
    }
}

/// A serial port choice: `auto`, `off` or a device path.
#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Ports {
    pub osdk_uart: String,
    pub osdk_baud: u32,
    pub usb_acm: String,
}

impl Default for Ports {
    fn default() -> Self {
        Self {
            osdk_uart: "auto".into(),
            osdk_baud: 230_400,
            usb_acm: "auto".into(),
        }
    }
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Motors {
    pub osdk_app_id: Option<u32>,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Gimbal {
    pub cmd_type: u8,
    pub plan_speed: i16,
}

impl Default for Gimbal {
    fn default() -> Self {
        Self {
            cmd_type: 0x00,
            plan_speed: 200,
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Video {
    /// The FPV camera's UDT address, or `off`.
    pub fpv_udt: String,
    /// Feed name (`cam1`, `cam2`, `fpv`) to RC mux channel.
    pub rc: BTreeMap<String, u8>,
    pub rc_dump: Option<PathBuf>,
}

impl Default for Video {
    fn default() -> Self {
        Self {
            fpv_udt: "192.168.42.2:40003".into(),
            rc: BTreeMap::from([("cam1".into(), 0x11), ("cam2".into(), 0x15)]),
            rc_dump: None,
        }
    }
}

impl Video {
    /// The FPV address, `None` when off.
    pub fn fpv(&self) -> Result<Option<SocketAddrV4>, String> {
        match self.fpv_udt.as_str() {
            "off" | "" => Ok(None),
            addr => addr
                .parse()
                .map(Some)
                .map_err(|e| format!("video.fpv_udt = {addr:?}: {e}")),
        }
    }
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Speech {
    pub provider: String,
    pub model: String,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Understanding {
    pub provider: String,
    pub model: String,
    pub local: LocalModel,
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LocalModel {
    pub enabled: bool,
    pub url: String,
    pub model: String,
}

impl Default for LocalModel {
    fn default() -> Self {
        Self {
            enabled: false,
            url: "http://127.0.0.1:8081".into(),
            model: String::new(),
        }
    }
}

#[derive(Debug, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsCfg {
    pub max_wait_s: u64,
    pub max_armed_s: u64,
    pub max_gimbal_s: u64,
    pub max_steps: usize,
}

impl Default for LimitsCfg {
    fn default() -> Self {
        let l = Limits::default();
        Self {
            max_wait_s: l.max_wait.as_secs(),
            max_armed_s: l.max_armed.as_secs(),
            max_gimbal_s: l.max_gimbal.as_secs(),
            max_steps: l.max_steps,
        }
    }
}

impl LimitsCfg {
    pub fn limits(&self) -> Limits {
        Limits {
            max_wait: Duration::from_secs(self.max_wait_s),
            max_armed: Duration::from_secs(self.max_armed_s),
            max_gimbal: Duration::from_secs(self.max_gimbal_s),
            max_steps: self.max_steps,
        }
    }
}

/// An empty string means "not set".
fn opt(s: &str) -> Option<&str> {
    (!s.is_empty()).then_some(s)
}

impl Config {
    /// The file `CONFIG` names (default [`DEFAULT_PATH`]), with the
    /// environment applied on top. A missing file gives the defaults; a
    /// file that does not parse is an error.
    pub fn load() -> Result<Self, String> {
        let path = PathBuf::from(std::env::var("CONFIG").unwrap_or_else(|_| DEFAULT_PATH.into()));
        let mut cfg = match std::fs::read_to_string(&path) {
            Ok(text) => {
                let cfg: Config =
                    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
                logln!("config: {}", path.display());
                cfg
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                logln!("config: no {}; using the defaults", path.display());
                Config::default()
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        cfg.anchor(path.parent().unwrap_or(Path::new("")));
        cfg.apply_env(|name| std::env::var(name).ok())?;
        Ok(cfg)
    }

    /// Make the file's relative paths relative to its directory.
    fn anchor(&mut self, dir: &Path) {
        for p in [
            &mut self.web.root,
            &mut self.web.cert_dir,
            &mut self.secrets.dir,
        ] {
            if p.is_relative() {
                *p = dir.join(&*p);
            }
        }
        if let Some(d) = &mut self.video.rc_dump
            && d.is_relative()
        {
            *d = dir.join(&*d);
        }
    }

    /// The per-run overrides: the variables the scripts and the README use.
    fn apply_env(&mut self, var: impl Fn(&str) -> Option<String>) -> Result<(), String> {
        let var = |name: &str| var(name).filter(|v| !v.trim().is_empty());
        if var("TLS").as_deref() == Some("off") {
            self.web.https = "off".into();
        }
        if let Some(v) = var("TLS_CERT_DIR") {
            self.web.cert_dir = v.into();
        }
        if let Some(v) = var("SECRETS_DIR") {
            self.secrets.dir = v.into();
        }
        if let Some(v) = var("OSDK_UART") {
            self.ports.osdk_uart = v;
        }
        if let Some(v) = var("OSDK_BAUD") {
            self.ports.osdk_baud = v.parse().map_err(|e| format!("OSDK_BAUD={v}: {e}"))?;
        }
        if let Some(v) = var("USB_ACM") {
            self.ports.usb_acm = v;
        }
        if let Some(v) = var("OSDK_APP_ID") {
            self.motors.osdk_app_id = Some(v.parse().map_err(|e| format!("OSDK_APP_ID={v}: {e}"))?);
        }
        if let Some(v) = var("GIMBAL_CMD_TYPE") {
            self.gimbal.cmd_type = u8::from_str_radix(v.trim().trim_start_matches("0x"), 16)
                .map_err(|e| format!("GIMBAL_CMD_TYPE={v}: {e}"))?;
        }
        if let Some(v) = var("FPV_UDT") {
            self.video.fpv_udt = v;
        }
        if let Some(v) = var("RC_VIDEO") {
            self.video.rc = parse_rc_video(&v)?;
        }
        if let Some(v) = var("RC_DUMP") {
            self.video.rc_dump = Some(v.into());
        }
        if let Some(v) = var("STT_PROVIDER") {
            self.speech.provider = v;
        }
        if let Some(v) = var("STT_MODEL") {
            self.speech.model = v;
        }
        if let Some(v) = var("LLM_PROVIDER") {
            self.understanding.provider = v;
        }
        if let Some(v) = var("LLM_MODEL") {
            self.understanding.model = v;
        }
        if let Some(v) = var("LOCAL_LLM_URL") {
            self.understanding.local.url = v;
            self.understanding.local.enabled = true;
        }
        if let Some(v) = var("LOCAL_LLM_MODEL") {
            self.understanding.local.model = v;
        }
        if self.understanding.provider == "local" {
            self.understanding.local.enabled = true;
        }
        Ok(())
    }

    /// The TLS listener's address, `None` when off.
    pub fn https(&self) -> Option<&str> {
        opt(&self.web.https).filter(|a| *a != "off")
    }
}

impl Speech {
    pub fn provider(&self) -> Option<&str> {
        opt(&self.provider)
    }
    pub fn model(&self) -> Option<&str> {
        opt(&self.model)
    }
}

impl Understanding {
    pub fn provider(&self) -> Option<&str> {
        opt(&self.provider)
    }
    pub fn model(&self) -> Option<&str> {
        opt(&self.model)
    }
}

impl LocalModel {
    pub fn model(&self) -> Option<&str> {
        opt(&self.model)
    }
}

/// `cam1=0x11,cam2=0x15[,fpv=0x13]`, or `off`.
fn parse_rc_video(text: &str) -> Result<BTreeMap<String, u8>, String> {
    let mut map = BTreeMap::new();
    if text.trim() == "off" {
        return Ok(map);
    }
    for entry in text.split(',') {
        let parsed = entry.split_once('=').and_then(|(name, ch)| {
            let name = name.trim();
            ["cam1", "cam2", "fpv"].contains(&name).then_some(())?;
            let ch = u8::from_str_radix(ch.trim().trim_start_matches("0x"), 16).ok()?;
            Some((name.to_string(), ch))
        });
        let Some((name, ch)) = parsed else {
            return Err(format!(
                "RC_VIDEO: bad entry {entry:?} (want cam1=0x11,cam2=0x15[,fpv=0x13])"
            ));
        };
        map.insert(name, ch);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checked_in_file_matches_the_defaults() {
        // Every value in config.toml is the default, so the file documents
        // the code and the code needs no file.
        let text =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../config.toml"))
                .unwrap();
        let cfg: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg, Config::default());
        assert!(toml::from_str::<Config>("[web]\nhttpz = 1").is_err());
    }

    #[test]
    fn the_development_file_only_tightens_the_limits() {
        // config.dev.toml must parse, keep every other default, and stay
        // within the flight limits it is a ground-test version of.
        let text = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../config.dev.toml"
        ))
        .unwrap();
        let cfg: Config = toml::from_str(&text).unwrap();
        let (dev, def) = (&cfg.limits, LimitsCfg::default());
        assert!(dev.max_wait_s <= def.max_wait_s && dev.max_armed_s <= def.max_armed_s);
        assert!(dev.max_gimbal_s <= def.max_gimbal_s && dev.max_steps <= def.max_steps);
        // docs/reverify.md H17 arms for 20 s.
        assert!(dev.max_wait_s >= 20 && dev.max_armed_s >= 20);
        let defaults = Config {
            limits: LimitsCfg::default(),
            ..cfg
        };
        assert_eq!(defaults, Config::default());
    }

    #[test]
    fn environment_overrides_the_file() {
        let mut cfg = Config::default();
        let env = |name: &str| {
            Some(
                match name {
                    "TLS" => "off",
                    "OSDK_APP_ID" => "42",
                    "GIMBAL_CMD_TYPE" => "0x40",
                    "RC_VIDEO" => "cam1=0x11,fpv=0x13",
                    "LOCAL_LLM_URL" => "http://localhost:1234",
                    "OSDK_UART" => "off",
                    _ => return None,
                }
                .to_string(),
            )
        };
        cfg.apply_env(env).unwrap();
        assert_eq!(cfg.https(), None);
        assert_eq!(cfg.motors.osdk_app_id, Some(42));
        assert_eq!(cfg.gimbal.cmd_type, 0x40);
        assert_eq!(cfg.video.rc.get("fpv"), Some(&0x13));
        assert_eq!(cfg.video.rc.get("cam2"), None);
        assert!(cfg.understanding.local.enabled);
        assert_eq!(cfg.ports.osdk_uart, "off");
        let mut bad = Config::default();
        assert!(
            bad.apply_env(|n| (n == "OSDK_BAUD").then(|| "fast".into()))
                .is_err()
        );
        assert!(parse_rc_video("cam9=0x11").is_err());
        assert!(parse_rc_video("off").unwrap().is_empty());
    }

    #[test]
    fn missing_keys_take_the_defaults_and_unknown_keys_are_errors() {
        for text in [
            "",
            "[web]",
            "[web]\n[limits]\n[understanding.local]",
            "[video]\nrc = {}",
        ] {
            let cfg: Config = toml::from_str(text).unwrap();
            assert_eq!(cfg.web, Web::default(), "{text:?}");
            assert_eq!(cfg.limits.limits(), Limits::default(), "{text:?}");
        }
        assert!(
            toml::from_str::<Config>("[video]\nrc = {}")
                .unwrap()
                .video
                .rc
                .is_empty()
        );
        for bad in [
            "colour = 1",
            "[webb]",
            "[web]\nport = 80",
            "[ports]\nbaud = 9600",
            "[motors]\napp_id = 1",
            "[limits]\nmax_wait = 5",
            "[understanding.local]\nkey = \"x\"",
            "[understanding.remote]",
            "[video]\nrc = { cam3 = 1 }\nrc_dumps = \"x\"",
        ] {
            assert!(toml::from_str::<Config>(bad).is_err(), "{bad:?}");
        }
        // Wrong types and out-of-range values are errors too, not defaults.
        for bad in [
            "[ports]\nosdk_baud = \"fast\"",
            "[gimbal]\ncmd_type = 256",
            "[gimbal]\nplan_speed = 40000",
            "[limits]\nmax_steps = -1",
            "[motors]\nosdk_app_id = 4294967296",
        ] {
            assert!(toml::from_str::<Config>(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn unparseable_overrides_are_errors_and_blank_ones_are_unset() {
        let fails = |name: &'static str, value: &'static str| {
            let mut cfg = Config::default();
            cfg.apply_env(|n| (n == name).then(|| value.to_string()))
                .is_err()
        };
        for (name, value) in [
            ("OSDK_APP_ID", "abc"),
            ("OSDK_APP_ID", "-1"),
            ("OSDK_APP_ID", "4294967296"),
            ("OSDK_BAUD", "230400.0"),
            ("GIMBAL_CMD_TYPE", "0x100"),
            ("GIMBAL_CMD_TYPE", "zz"),
            ("RC_VIDEO", "cam1"),
            ("RC_VIDEO", "cam1=0xZZ"),
            ("RC_VIDEO", "cam1=0x11,,cam2=0x15"),
            ("RC_VIDEO", "cam1=0x111"),
        ] {
            assert!(fails(name, value), "{name}={value}");
        }
        let mut cfg = Config::default();
        cfg.apply_env(|n| match n {
            "OSDK_APP_ID" | "LLM_PROVIDER" => Some("   ".into()),
            "TLS" => Some("on".into()),
            "GIMBAL_CMD_TYPE" => Some("40".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(cfg.motors.osdk_app_id, None, "blank is unset");
        assert_eq!(cfg.understanding.provider(), None);
        assert!(cfg.https().is_some(), "only TLS=off turns it off");
        assert_eq!(cfg.gimbal.cmd_type, 0x40, "hex with or without 0x");
        assert!(
            Video {
                fpv_udt: "orin:1".into(),
                ..Video::default()
            }
            .fpv()
            .is_err()
        );
        assert_eq!(
            Video {
                fpv_udt: "off".into(),
                ..Video::default()
            }
            .fpv(),
            Ok(None)
        );
    }

    #[test]
    fn relative_paths_follow_the_file() {
        let mut cfg = Config::default();
        cfg.anchor(Path::new("/opt/dji"));
        assert_eq!(cfg.web.root, PathBuf::from("/opt/dji/web/dist"));
        assert_eq!(cfg.secrets.dir, PathBuf::from("/opt/dji/secrets"));
        assert_eq!(cfg.limits.limits(), Limits::default());
        assert_eq!(cfg.video.fpv().unwrap().map(|a| a.port()), Some(40003));
    }
}
