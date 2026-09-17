//! Read only literal leading shell words; never evaluate expansions or scripts.

struct Word {
    value: String,
    assignment: bool,
}

fn assignment(value: &str) -> bool {
    value.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.bytes().enumerate().all(|(i, byte)| {
                byte == b'_' || byte.is_ascii_alphabetic() || (i > 0 && byte.is_ascii_digit())
            })
    })
}

fn word(input: &mut &str) -> Option<Word> {
    let text = input.trim_start_matches([' ', '\t', '\r']);
    let mut chars = text.char_indices();
    let mut value = String::new();
    let mut quote = None;
    let mut plain_name = true;
    let mut is_assignment = false;
    while let Some((index, ch)) = chars.next() {
        match (quote, ch) {
            (
                None,
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '<' | '>' | '(' | ')' | '{' | '}',
            ) => {
                *input = &text[index..];
                return (!value.is_empty()).then_some(Word {
                    value,
                    assignment: is_assignment,
                });
            }
            (None, '\'' | '"') => {
                quote = Some(ch);
                plain_name = false;
            }
            (Some(q), c) if q == c => quote = None,
            (None | Some('"'), '$' | '`') | (None, '*' | '?' | '~') => return None,
            (None, '[')
                if !value.is_empty()
                    || !chars.clone().next().is_none_or(|(_, c)| c.is_whitespace()) =>
            {
                return None;
            }
            (None | Some('"'), '\\') => {
                let (_, escaped) = chars.next()?;
                if escaped != '\n' {
                    // Inside double quotes only these characters lose the slash.
                    if quote.is_some() && !"$`\"\\".contains(escaped) {
                        value.push('\\');
                    }
                    value.push(escaped);
                    plain_name = false;
                }
            }
            _ => {
                value.push(ch);
                if ch == '=' && quote.is_none() && plain_name && assignment(&value) {
                    is_assignment = true;
                }
            }
        }
    }
    *input = "";
    (quote.is_none() && !value.is_empty()).then_some(Word {
        value,
        assignment: is_assignment,
    })
}

fn wrapper_command<'a>(name: &str, input: &'a str) -> Option<(&'a str, &'static str)> {
    let mut rest = input;
    let wrapper = match name {
        "rtk" => {
            let token = word(&mut rest)?.value;
            if token.starts_with('-') {
                return None;
            }
            if token == "proxy" {
                return Some((rest, "rtk proxy"));
            }
            return Some((input, "rtk"));
        }
        "sudo" => "sudo",
        "env" => "env",
        "command" => "command",
        "exec" => "exec",
        "timeout" => "timeout",
        _ => return None,
    };
    let mut options = true;
    loop {
        let before = rest;
        let token = word(&mut rest)?.value;
        if options && token == "--" {
            options = false;
            continue;
        }
        if options && token.starts_with('-') {
            let (flag, attached) = token
                .split_once('=')
                .map_or((token.as_str(), false), |(f, _)| (f, true));
            let (switches, operands): (&[&str], &[&str]) = match name {
                "sudo" => (
                    &[
                        "-n",
                        "-E",
                        "-H",
                        "--non-interactive",
                        "--preserve-env",
                        "--set-home",
                    ],
                    &["-u", "-g", "--user", "--group", "--chdir"],
                ),
                "env" => (
                    &["-i", "--ignore-environment"],
                    &["-u", "--unset", "-C", "--chdir"],
                ),
                "command" => (&["-p"], &[]),
                "exec" => (&["-c", "-l"], &["-a"]),
                "timeout" => (
                    &["--foreground", "--preserve-status", "--verbose"],
                    &["-k", "--kill-after", "-s", "--signal"],
                ),
                _ => return None,
            };
            if operands.contains(&flag) {
                if !attached {
                    word(&mut rest)?;
                }
            } else if attached || !switches.contains(&flag) {
                return None;
            }
            continue;
        }
        if ["env", "sudo"].contains(&name) && assignment(&token) {
            options = false;
            continue;
        }
        if name == "timeout" {
            // Only a literal duration is consumed. Unknown syntax keeps timeout.
            let duration = token.strip_suffix(['s', 'm', 'h', 'd']).unwrap_or(&token);
            if duration
                .parse::<f64>()
                .is_ok_and(|n| n.is_finite() && n >= 0.0)
            {
                return Some((rest, wrapper));
            }
            return None;
        }
        return Some((before, wrapper));
    }
}

pub(super) fn leading<'a>(
    command: &'a str,
    wrapper: &mut Option<String>,
) -> Option<(String, &'a str)> {
    let mut rest = command.trim_start();
    while rest.starts_with('#') {
        rest = rest.split_once('\n')?.1.trim_start();
    }
    // ponytail: literal prefixes only; dynamic shell syntax remains opaque.
    let mut shell_assignments = true;
    loop {
        let token = word(&mut rest)?;
        if token.assignment {
            if shell_assignments {
                continue;
            }
            return None;
        }
        shell_assignments = false;
        let name = token.value.rsplit('/').next()?;
        if let Some((nested, label)) = wrapper_command(name, rest) {
            let mut probe = nested;
            if word(&mut probe).is_some() {
                if wrapper.is_none() {
                    *wrapper = Some(label.to_owned());
                }
                rest = nested;
                continue;
            }
        }
        return Some((name.to_owned(), rest));
    }
}

pub(super) fn subcommand(input: &mut &str) -> Option<String> {
    word(input).map(|w| w.value)
}
