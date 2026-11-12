// SPDX-License-Identifier: AGPL-3.0-or-later
use super::*;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressTopologyDeclaration")]
pub struct TopologyDeclaration {
    #[serde(default)]
    pub aliases: Vec<RoleAlias>,
    #[serde(default)]
    pub bus: String,
    #[serde(default)]
    pub wiring: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(rename = "StressRoleAlias")]
pub struct RoleAlias {
    pub channel: String,
    pub alias: String,
}
impl TopologyDeclaration {
    pub fn validate(&self, p: &Plan) -> Result<(), String> {
        let valid = |s: &str, max: usize| s.len() <= max && !s.chars().any(char::is_control);
        let mut channels = HashSet::new();
        let mut aliases = HashSet::new();
        if !valid(&self.bus, 128) || !valid(&self.wiring, 1024) || self.aliases.len() > 8 {
            return Err("topology declaration exceeds text/role bounds".into());
        }
        for a in &self.aliases {
            if a.alias.trim().is_empty()
                || !valid(&a.alias, 64)
                || !channels.insert(&a.channel)
                || !aliases.insert(&a.alias)
                || !p.channels().contains(&a.channel)
            {
                return Err("topology aliases must be unique selected roles, 1..64 bytes".into());
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressSourceFile")]
pub struct SourceFile {
    pub name: String,
    pub sha256: Option<String>,
    pub bytes: Option<u64>,
    pub status: String,
}
impl From<canharbor_core::measurement::SourceEvidence> for SourceFile {
    fn from(v: canharbor_core::measurement::SourceEvidence) -> Self {
        Self {
            name: v.name,
            sha256: v.sha256,
            bytes: v.bytes,
            status: v.status,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressChannelSource")]
pub struct ChannelSource {
    pub channel: String,
    pub device_id: String,
    pub backend: String,
    pub simulated: bool,
    pub libraries: Vec<SourceFile>,
    pub firmware: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[schemars(rename = "StressProvenance")]
pub struct Provenance {
    pub package_version: String,
    pub os: String,
    pub architecture: String,
    pub executable: SourceFile,
    pub channels: Vec<ChannelSource>,
    pub scope: String,
}
impl Provenance {
    pub fn valid(&self, plan: &Plan) -> bool {
        let bounded = |s: &str, n: usize| s.len() <= n && !s.chars().any(char::is_control);
        let file = |s: &SourceFile| {
            bounded(&s.name, 128)
                && bounded(&s.status, 256)
                && s.bytes.is_none_or(|n| n <= 512 * 1024 * 1024)
                && s.sha256
                    .as_ref()
                    .is_none_or(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
                && s.sha256.is_some() == s.bytes.is_some()
        };
        bounded(&self.package_version, 128)
            && bounded(&self.os, 64)
            && bounded(&self.architecture, 64)
            && bounded(&self.scope, 256)
            && file(&self.executable)
            && self.channels.len() == plan.channels().len()
            && self
                .channels
                .iter()
                .map(|c| &c.channel)
                .collect::<std::collections::BTreeSet<_>>()
                == plan
                    .channels()
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
            && self.channels.iter().all(|c| {
                bounded(&c.device_id, 256)
                    && bounded(&c.backend, 64)
                    && bounded(&c.firmware, 128)
                    && c.libraries.len() <= 16
                    && c.libraries.iter().all(file)
            })
    }

    pub fn software() -> Self {
        static SOURCE: std::sync::OnceLock<SourceFile> = std::sync::OnceLock::new();
        Self { package_version:env!("CARGO_PKG_VERSION").into(),os:std::env::consts::OS.into(),architecture:std::env::consts::ARCH.into(), executable:SOURCE.get_or_init(executable).clone(),channels:vec![],scope:"disk_source_evidence; not signatures, memory image or hardware certification; no automatic topology verification".into() }
    }
}
fn executable() -> SourceFile {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut source = SourceFile {
        name: "service_executable".into(),
        sha256: None,
        bytes: None,
        status: "unavailable".into(),
    };
    let result = (|| -> std::io::Result<(u64, String)> {
        let mut f = canharbor_platform::files::open_regular(&std::env::current_exe()?)?;
        let before = canharbor_platform::files::stamp(&f)?;
        let a = f.metadata()?;
        if !a.is_file() || a.len() > 512 * 1024 * 1024 {
            return Err(std::io::Error::other("source bounds"));
        }
        let mut h = Sha256::new();
        let mut bytes = 0;
        let mut buffer = [0; 65536];
        loop {
            let n = f.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > a.len() {
                return Err(std::io::Error::other("source grew"));
            }
            h.update(&buffer[..n]);
        }
        if bytes != a.len() || before != canharbor_platform::files::stamp(&f)? {
            return Err(std::io::Error::other("source changed"));
        }
        Ok((
            bytes,
            h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        ))
    })();
    if let Ok((bytes, hash)) = result {
        source.bytes = Some(bytes);
        source.sha256 = Some(hash);
        source.status =
            "sha256_disk_executable_at_first_capture; memory_and_build_source_unverified".into();
    }
    source
}
impl Report {
    pub fn capture_sources(&mut self, runtime: &Runtime) {
        let mut p = Provenance::software();
        for c in &self.plan.channels() {
            if let Ok(caps) = runtime.capabilities(c) {
                p.channels.push(ChannelSource {
                    channel: c.clone(),
                    device_id: caps.device_id,
                    backend: caps.backend,
                    simulated: caps.simulated,
                    libraries: runtime
                        .source_evidence(c)
                        .into_iter()
                        .take(16)
                        .map(Into::into)
                        .collect(),
                    firmware: "unknown; not queried by stress".into(),
                });
            }
        }
        if let Some(g) = &mut self.group {
            g.budget.bind_sources(&p);
        }
        self.provenance = Some(p);
    }
}
