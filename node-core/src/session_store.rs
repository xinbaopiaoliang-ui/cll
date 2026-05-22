use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Default)]
pub struct SessionStore {
    udp_sessions: Mutex<HashMap<String, UdpSession>>,
}

#[derive(Debug, Clone)]
pub struct UdpSession {
    pub session_id: String,
    pub user_id: Option<u64>,
    pub device_id: Option<String>,
    pub game_id: Option<u64>,
    pub authenticated: bool,
    pub created_at: u64,
    pub expires_at: u64,
    pub last_seen_at: u64,
    pub last_peer: SocketAddr,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct UdpSessionSnapshot {
    pub session_id: String,
    pub user_id: Option<u64>,
    pub device_id: Option<String>,
    pub game_id: Option<u64>,
    pub authenticated: bool,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpSessionError {
    Missing,
    Expired,
    LockPoisoned,
}

impl UdpSession {
    pub fn new(
        session_id: String,
        user_id: Option<u64>,
        device_id: Option<String>,
        game_id: Option<u64>,
        authenticated: bool,
        ttl_sec: u64,
        peer: SocketAddr,
    ) -> Self {
        let now = now_unix();

        Self {
            session_id,
            user_id,
            device_id,
            game_id,
            authenticated,
            created_at: now,
            expires_at: now + ttl_sec,
            last_seen_at: now,
            last_peer: peer,
            rx_bytes: 0,
            tx_bytes: 0,
        }
    }
}

impl SessionStore {
    pub fn register_udp_session(&self, session: UdpSession) {
        if let Ok(mut sessions) = self.udp_sessions.lock() {
            remove_expired(&mut sessions, now_unix());
            sessions.insert(session.session_id.clone(), session);
        }
    }

    pub fn record_udp_session_io(
        &self,
        session_id: &str,
        peer: SocketAddr,
        rx_bytes: u64,
        tx_bytes: u64,
    ) -> Result<UdpSessionSnapshot, UdpSessionError> {
        let mut sessions = self
            .udp_sessions
            .lock()
            .map_err(|_| UdpSessionError::LockPoisoned)?;
        let now = now_unix();
        let Some(session) = sessions.get_mut(session_id) else {
            return Err(UdpSessionError::Missing);
        };

        if session.expires_at <= now {
            sessions.remove(session_id);
            return Err(UdpSessionError::Expired);
        }

        session.last_seen_at = now;
        session.last_peer = peer;
        session.rx_bytes = session.rx_bytes.saturating_add(rx_bytes);
        session.tx_bytes = session.tx_bytes.saturating_add(tx_bytes);

        Ok(session.snapshot())
    }

    pub fn active_udp_session_count(&self) -> u64 {
        let Ok(mut sessions) = self.udp_sessions.lock() else {
            return 0;
        };
        remove_expired(&mut sessions, now_unix());
        sessions.len() as u64
    }
}

impl UdpSession {
    fn snapshot(&self) -> UdpSessionSnapshot {
        UdpSessionSnapshot {
            session_id: self.session_id.clone(),
            user_id: self.user_id,
            device_id: self.device_id.clone(),
            game_id: self.game_id,
            authenticated: self.authenticated,
            created_at: self.created_at,
            expires_at: self.expires_at,
        }
    }
}

fn remove_expired(sessions: &mut HashMap<String, UdpSession>, now: u64) {
    sessions.retain(|_, session| session.expires_at > now);
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_udp_session_io() {
        let store = SessionStore::default();
        let peer = "127.0.0.1:12345".parse::<SocketAddr>().unwrap();
        store.register_udp_session(UdpSession::new(
            "s1".to_string(),
            Some(1001),
            Some("pc-001".to_string()),
            Some(8888),
            true,
            30,
            peer,
        ));

        let session = store
            .record_udp_session_io("s1", peer, 5, 7)
            .expect("session exists");

        assert_eq!(session.session_id, "s1");
        assert_eq!(session.user_id, Some(1001));
        assert!(session.authenticated);
        assert_eq!(store.active_udp_session_count(), 1);
    }

    #[test]
    fn misses_unknown_udp_session() {
        let store = SessionStore::default();
        let peer = "127.0.0.1:12345".parse::<SocketAddr>().unwrap();

        assert_eq!(
            store
                .record_udp_session_io("missing", peer, 1, 1)
                .unwrap_err(),
            UdpSessionError::Missing
        );
    }
}
