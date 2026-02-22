use std::collections::HashSet;

pub async fn detect_ports(pid: u32) -> Vec<u16> {
    let output = match tokio::process::Command::new("lsof")
        .args(["-i", "-P", "-n", "-a", "-p", &pid.to_string()])
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return Vec::new(),
    };

    if !output.status.success() {
        return Vec::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
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

    ports.into_iter().collect()
}
