//! 调用方速率限制（规格书安全模型第 6 条）。固定窗口计数，M0 够用；
//! 需要平滑突发时再换令牌桶。

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::error::PolicyError;

pub type CallerId = String;

struct Window {
    count: u32,
    started: Instant,
}

pub struct RateLimiter {
    per_minute: u32,
    windows: Mutex<HashMap<CallerId, Window>>,
}

impl RateLimiter {
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    pub fn check(&self, caller: &str) -> Result<(), PolicyError> {
        let mut map = self.windows.lock().unwrap_or_else(|e| e.into_inner());
        let w = map.entry(caller.to_string()).or_insert(Window {
            count: 0,
            started: Instant::now(),
        });
        if w.started.elapsed() >= Duration::from_secs(60) {
            w.count = 0;
            w.started = Instant::now();
        }
        if w.count >= self.per_minute {
            return Err(PolicyError::RateLimited {
                limit: self.per_minute,
            });
        }
        w.count += 1;
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_limit_then_rejects() {
        let rl = RateLimiter::new(2);
        assert!(rl.check("agent-a").is_ok());
        assert!(rl.check("agent-a").is_ok());
        assert!(rl.check("agent-a").is_err());
        // 不同调用方独立计数
        assert!(rl.check("agent-b").is_ok());
    }
}
