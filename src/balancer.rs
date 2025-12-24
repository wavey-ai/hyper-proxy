use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadBalancingMode {
    LeastConn,
    Queue,
}

impl LoadBalancingMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            LoadBalancingMode::LeastConn => "leastconn",
            LoadBalancingMode::Queue => "queue",
        }
    }
}

impl FromStr for LoadBalancingMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.trim().to_ascii_lowercase().as_str() {
            "leastconn" | "least_conn" | "least" => Ok(LoadBalancingMode::LeastConn),
            "queue" | "queued" => Ok(LoadBalancingMode::Queue),
            other => Err(format!("unsupported load balancer: {other}")),
        }
    }
}
