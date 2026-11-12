// SPDX-License-Identifier: AGPL-3.0-or-later
//! Presentation only. Commands, protocol values and machine output are unchanged.
use std::{
    collections::BTreeMap,
    ffi::OsString,
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};
static CHINESE: AtomicBool = AtomicBool::new(false);
static HELP: OnceLock<BTreeMap<String, String>> = OnceLock::new();
fn catalog() -> &'static BTreeMap<String, String> {
    HELP.get_or_init(|| {
        serde_json::from_str(include_str!("locales/zh-CN.json")).expect("embedded CLI catalog")
    })
}
fn translated(value: &str) -> Option<&'static str> {
    catalog()
        .get(value)
        .or_else(|| {
            catalog()
                .iter()
                .find(|(key, _)| key.trim_end_matches('.') == value.trim_end_matches('.'))
                .map(|(_, value)| value)
        })
        .map(String::as_str)
}
pub fn chinese() -> bool {
    CHINESE.load(Ordering::Relaxed)
}
/// A read-only bootstrap scan; clap remains responsible for argument validation.
fn explicit(args: impl IntoIterator<Item = OsString>) -> Option<String> {
    let mut args = args.into_iter().skip(1);
    let mut selected = None;
    while let Some(arg) = args.next() {
        if arg == "--" {
            break;
        }
        if arg == "--lang" {
            selected = args.next().and_then(|value| value.into_string().ok());
        } else if let Some(value) = arg.to_str().and_then(|s| s.strip_prefix("--lang=")) {
            selected = Some(value.to_owned());
        }
    }
    selected
}
fn resolve(requested: Option<&str>, saved: &str, environment: &str) -> bool {
    let choice = requested.filter(|s| *s != "auto").unwrap_or(saved);
    match choice {
        "zh-CN" => true,
        "en" => false,
        _ => {
            let env = environment.to_ascii_lowercase();
            env == "zh" || env.starts_with("zh_") || env.starts_with("zh-")
        }
    }
}
pub fn initialize() {
    let requested = explicit(std::env::args_os());
    let saved =
        canharbor_server::preferences::Store::new(canharbor_server::preferences::default_path())
            .get()
            .locale;
    let environment = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok().filter(|v| !v.is_empty()))
        .unwrap_or_default();
    CHINESE.store(
        resolve(requested.as_deref(), saved.as_str(), &environment),
        Ordering::Relaxed,
    );
}
pub fn command(mut command: clap::Command) -> clap::Command {
    if !chinese() {
        return command;
    }
    command.build();
    if let Some(value) = command.get_about().and_then(|s| translated(&s.to_string())) {
        command = command.about(value);
    }
    if let Some(value) = command
        .get_long_about()
        .and_then(|s| translated(&s.to_string()))
    {
        command = command.long_about(value);
    }
    if let Some(value) = command
        .get_after_help()
        .and_then(|s| translated(&s.to_string()))
    {
        command = command.after_help(value);
    }
    command = command.subcommand_help_heading("命令").help_template(
        "{before-help}{about-with-newline}\n用法：{usage}\n\n{all-args}{after-help}",
    );
    command = command.mut_args(|mut arg| {
        if let Some(value) = arg.get_help().and_then(|s| translated(&s.to_string())) {
            arg = arg.help(value);
        }
        if let Some(value) = arg.get_long_help().and_then(|s| translated(&s.to_string())) {
            arg = arg.long_help(value);
        }
        let heading = if arg.is_positional() {
            "参数"
        } else {
            "选项"
        };
        arg.help_heading(heading)
    });
    for child in command.get_subcommands_mut() {
        *child = self::command(child.clone());
    }
    command
}
pub fn error_summary(code: i32) -> &'static str {
    match code {
        2 => "参数或请求无效。请检查参数、访问权限和通道设置。",
        3 => "无法连接本地服务或事件流。请检查服务状态。",
        4 => "设备、通道或结果不可用。请检查设备连接与资源标识。",
        5 => "资源状态或控制权冲突。请先查询当前状态。",
        7 => "操作结果未知。请使用原始请求标识查询结果，不要自动重试或重发。",
        8 => "已达到资源上限。请检查配额、存储及活动任务。",
        _ => "操作失败。请检查技术详情与当前状态。",
    }
}
pub fn report_error(error: &(dyn std::error::Error + 'static)) {
    if chinese() {
        eprintln!(
            "{}\n技术详情：",
            error_summary(crate::status::exit_code(error))
        );
    }
    eprintln!("{error}");
}
pub fn choose<'a>(en: &'a str, zh: &'a str) -> &'a str {
    if chinese() { zh } else { en }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locale_priority_and_argument_boundaries() {
        assert!(resolve(Some("zh-CN"), "en", "C"));
        assert!(!resolve(Some("en"), "zh-CN", "zh_CN.UTF-8"));
        assert!(resolve(Some("auto"), "zh-CN", "C"));
        assert!(resolve(None, "auto", "zh_TW.UTF-8"));
        assert!(!resolve(None, "auto", "fr_FR.UTF-8"));
        let scan = |values: &[&str]| explicit(values.iter().map(OsString::from));
        assert_eq!(
            scan(&["canharbor", "devices", "--lang=zh-CN"]).as_deref(),
            Some("zh-CN")
        );
        assert_eq!(
            scan(&["canharbor", "--lang", "en", "--", "--lang=zh-CN"]).as_deref(),
            Some("en")
        );
    }
    #[test]
    fn every_authored_help_message_has_a_translation() {
        use clap::CommandFactory;
        fn check(command: clap::Command) {
            for value in [
                command.get_about(),
                command.get_long_about(),
                command.get_after_help(),
            ]
            .into_iter()
            .flatten()
            {
                assert!(
                    translated(&value.to_string()).is_some(),
                    "{}: {}",
                    command.get_name(),
                    value
                );
            }
            for arg in command.get_arguments() {
                for value in [arg.get_help(), arg.get_long_help()].into_iter().flatten() {
                    assert!(
                        translated(&value.to_string()).is_some(),
                        "{} / {}: {}",
                        command.get_name(),
                        arg.get_id(),
                        value
                    );
                }
            }
            for child in command.get_subcommands() {
                check(child.clone());
            }
        }
        check(crate::Args::command());
    }
}
