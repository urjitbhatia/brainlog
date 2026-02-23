use std::collections::HashSet;

pub async fn detect_ports(pid: u32) -> Vec<u16> {
    let pids = collect_process_tree(pid).await;
    if pids.is_empty() {
        return Vec::new();
    }

    let pid_arg = pids
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let output = match tokio::process::Command::new("lsof")
        .args(["-i", "-P", "-n", "-a", "-p", &pid_arg])
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    parse_lsof_ports(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .collect()
}

/// Collect all PIDs in the process tree rooted at `pid` (inclusive).
async fn collect_process_tree(pid: u32) -> Vec<u32> {
    let mut all_pids = vec![pid];
    let mut to_visit = vec![pid];

    while let Some(parent) = to_visit.pop() {
        if let Ok(output) = tokio::process::Command::new("pgrep")
            .args(["-P", &parent.to_string()])
            .output()
            .await
        {
            if output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                for line in stdout.lines() {
                    if let Ok(child_pid) = line.trim().parse::<u32>() {
                        if !all_pids.contains(&child_pid) {
                            all_pids.push(child_pid);
                            to_visit.push(child_pid);
                        }
                    }
                }
            }
        }
    }

    all_pids
}

fn parse_lsof_ports(stdout: &str) -> HashSet<u16> {
    let mut ports = HashSet::new();

    for line in stdout.lines().skip(1) {
        // lsof output: COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME
        // NAME is like: *:8080 (LISTEN) or 127.0.0.1:3000 (LISTEN)
        if !line.contains("LISTEN") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(name) = parts.last() {
            // Handle "(LISTEN)" being separate or attached
            let name_part = if *name == "(LISTEN)" {
                parts
                    .get(parts.len().wrapping_sub(2))
                    .copied()
                    .unwrap_or("")
            } else {
                name
            };
            if let Some(port_str) = name_part.rsplit(':').next() {
                if let Ok(port) = port_str.parse::<u16>() {
                    ports.insert(port);
                }
            }
        }
    }

    ports
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lsof_typical_output() {
        let output = "\
COMMAND   PID USER   FD   TYPE             DEVICE SIZE/OFF NODE NAME
node    12345 user   22u  IPv6 0x1234567890abcdef      0t0  TCP *:3000 (LISTEN)
node    12345 user   23u  IPv4 0xfedcba0987654321      0t0  TCP 127.0.0.1:9229 (LISTEN)
node    12345 user   24u  IPv4 0xabcdef1234567890      0t0  TCP 192.168.1.1:3000->10.0.0.1:51234 (ESTABLISHED)";
        let ports = parse_lsof_ports(output);
        assert_eq!(ports.len(), 2);
        assert!(ports.contains(&3000));
        assert!(ports.contains(&9229));
    }

    #[test]
    fn parse_lsof_empty_output() {
        assert!(parse_lsof_ports("").is_empty());
        assert!(
            parse_lsof_ports("COMMAND PID USER FD TYPE DEVICE SIZE/OFF NODE NAME\n").is_empty()
        );
    }
}
