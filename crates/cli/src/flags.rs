//! Flags as Go's flag package takes them, which is how the CLI has always
//! been driven: `-name value`, `-name=value` or `--name`, a boolean flag
//! alone, all before the arguments, and `-h` for a command's flags.

use std::collections::HashMap;

/// One flag a command takes.
pub struct Flag {
    pub name: &'static str,
    /// A switch, set by naming it.
    pub switch: bool,
    pub default: String,
    pub usage: &'static str,
}

impl Flag {
    pub fn value(name: &'static str, default: impl Into<String>, usage: &'static str) -> Flag {
        Flag {
            name,
            switch: false,
            default: default.into(),
            usage,
        }
    }

    pub fn switch(name: &'static str, usage: &'static str) -> Flag {
        Flag {
            name,
            switch: true,
            default: "false".into(),
            usage,
        }
    }
}

/// What a command's flags were set to.
pub struct Parsed {
    values: HashMap<&'static str, String>,
}

impl Parsed {
    pub fn get(&self, name: &str) -> &str {
        self.values
            .get(name)
            .map(String::as_str)
            .unwrap_or_default()
    }

    pub fn on(&self, name: &str) -> bool {
        self.get(name) == "true"
    }

    pub fn number(&self, name: &str) -> Result<u32, String> {
        self.get(name)
            .parse()
            .map_err(|_| format!("-{name} takes a number, not {:?}", self.get(name)))
    }
}

/// How parsing ended without flags to run with.
pub enum Stop {
    /// `-h`: the usage was printed, and the command succeeds.
    Help,
    /// A flag was wrong; the usage was printed after why.
    Wrong,
}

fn usage(command: &str, flags: &[Flag]) -> String {
    let mut out = format!("Usage of {command}:\n");
    for f in flags {
        let kind = if f.switch { "" } else { " string" };
        out.push_str(&format!("  -{}{kind}\n    \t{}", f.name, f.usage));
        if !f.switch && !f.default.is_empty() {
            out.push_str(&format!(" (default {:?})", f.default));
        }
        out.push('\n');
    }
    out
}

/// Parses a command's arguments against its flags.
pub fn parse(command: &str, flags: &[Flag], args: &[String]) -> Result<Parsed, Stop> {
    let mut values: HashMap<&'static str, String> =
        flags.iter().map(|f| (f.name, f.default.clone())).collect();
    let wrong = |why: String| {
        eprint!("{why}\n{}", usage(command, flags));
        Stop::Wrong
    };
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let Some(rest) = a.strip_prefix('-') else {
            return Err(wrong(format!("unexpected argument {a:?}")));
        };
        if rest == "-" {
            i += 1;
            if i < args.len() {
                return Err(wrong(format!("unexpected argument {:?}", args[i])));
            }
            break;
        }
        let rest = rest.strip_prefix('-').unwrap_or(rest);
        let (name, given) = match rest.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (rest, None),
        };
        if name == "h" || name == "help" {
            print!("{}", usage(command, flags));
            return Err(Stop::Help);
        }
        let Some(f) = flags.iter().find(|f| f.name == name) else {
            return Err(wrong(format!("flag provided but not defined: -{name}")));
        };
        let value = match (f.switch, given) {
            (true, None) => "true".to_string(),
            (true, Some(v)) => match v.as_str() {
                "true" | "1" | "t" | "T" | "TRUE" | "True" => "true".into(),
                "false" | "0" | "f" | "F" | "FALSE" | "False" => "false".into(),
                _ => return Err(wrong(format!("invalid boolean value {v:?} for -{name}"))),
            },
            (false, Some(v)) => v,
            (false, None) => {
                i += 1;
                match args.get(i) {
                    Some(v) => v.clone(),
                    None => return Err(wrong(format!("flag needs an argument: -{name}"))),
                }
            }
        };
        values.insert(f.name, value);
        i += 1;
    }
    Ok(Parsed { values })
}
