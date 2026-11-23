use zed_extension_api::{Architecture, Extension, Os, Result, current_platform, serde_json::json};

struct Copilot;

impl Extension for Copilot {
    fn new() -> Self {
        Self
    }

    fn run_command(&mut self, id: &str, _active_file: Option<&str>) -> Result<Option<String>> {
        match id {
            "sign-in" | "sign-out" | "enable" | "disable" | "pause" | "resume" | "suggest" | "toggle-language" => {
                Ok(Some(id.to_owned()))
            }
            "server-release" => {
                let (platform, sha256) = match current_platform() {
                    (Os::Mac, Architecture::Aarch64) => (
                        "darwin-arm64",
                        "a0b6377a22ebf2dca9e80adbb26c7dd61f5b867233050635e0b1ac0868a6bd75",
                    ),
                    (Os::Mac, Architecture::X8664) => (
                        "darwin-x64",
                        "935358bdd66ab8b470808debac1c4222e2a1ea733215c599008378e9a9e754dc",
                    ),
                    _ => return Err("Set copilot.server_path to use an installed Copilot server.".into()),
                };
                Ok(Some(
                    json!({"version": "1.567.1", "platform": platform, "sha256": sha256}).to_string(),
                ))
            }
            _ => Err(format!("Unknown GitHub Copilot command {id}")),
        }
    }
}

zed_extension_api::register_extension!(Copilot);
