//! 极简参数解析：`--key value`、`--key=value`、`--flag`，其余是位置参数。不引入 clap，工具程序用不着。

use std::collections::{HashMap, HashSet};
use std::str::FromStr;

#[derive(Debug, Default)]
pub struct Args {
    pub pos: Vec<String>,
    opts: HashMap<String, String>,
    flags: HashSet<String>,
}

impl Args {
    pub fn parse(items: impl IntoIterator<Item = String>) -> Self {
        let mut a = Args::default();
        let mut it = items.into_iter().peekable();
        while let Some(item) = it.next() {
            let Some(key) = item.strip_prefix("--") else {
                a.pos.push(item);
                continue;
            };
            if let Some((k, v)) = key.split_once('=') {
                a.opts.insert(k.to_string(), v.to_string());
            } else if it.peek().is_some_and(|n| !n.starts_with("--")) {
                a.opts.insert(key.to_string(), it.next().unwrap());
            } else {
                a.flags.insert(key.to_string());
            }
        }
        a
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.opts.get(key).map(String::as_str)
    }

    pub fn flag(&self, key: &str) -> bool {
        self.flags.contains(key)
    }

    pub fn num<T: FromStr>(&self, key: &str, default: T) -> anyhow::Result<T> {
        match self.get(key) {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| anyhow::anyhow!("参数 --{key} 的值“{v}”不是有效的数字")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> Args {
        Args::parse(s.split_whitespace().map(String::from))
    }

    #[test]
    fn parses_positionals_options_and_flags() {
        let a = p("s1 capture --secs 20 --out=D:\\x --keepalive --mic communications");
        assert_eq!(a.pos, vec!["s1", "capture"]);
        assert_eq!(a.get("secs"), Some("20"));
        assert_eq!(a.get("out"), Some("D:\\x"));
        assert!(a.flag("keepalive"));
        assert_eq!(a.get("mic"), Some("communications"));
        assert!(!a.flag("secs"));
    }

    #[test]
    fn a_flag_followed_by_another_option_stays_a_flag() {
        let a = p("--keepalive --secs 5");
        assert!(a.flag("keepalive"));
        assert_eq!(a.get("secs"), Some("5"));
    }

    #[test]
    fn numbers_have_defaults_and_report_bad_values() {
        let a = p("--secs 12 --bad x");
        assert_eq!(a.num("secs", 1u64).unwrap(), 12);
        assert_eq!(a.num("missing", 7u64).unwrap(), 7);
        assert!(a.num::<u64>("bad", 0).unwrap_err().to_string().contains("--bad"));
    }
}
