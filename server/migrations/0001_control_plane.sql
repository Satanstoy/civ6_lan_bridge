CREATE TABLE rooms (
    room_id UUID PRIMARY KEY,
    room_code TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ
);

CREATE TABLE peers (
    peer_id UUID PRIMARY KEY,
    room_id UUID NOT NULL REFERENCES rooms(room_id) ON DELETE CASCADE,
    virtual_ip INET NOT NULL UNIQUE,
    wireguard_public_key TEXT NOT NULL UNIQUE,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    left_at TIMESTAMPTZ
);

CREATE INDEX peers_room_id_idx ON peers(room_id);

CREATE TABLE host_sessions (
    host_session_id UUID PRIMARY KEY,
    room_id UUID NOT NULL REFERENCES rooms(room_id) ON DELETE CASCADE,
    peer_id UUID NOT NULL REFERENCES peers(peer_id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_heartbeat_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    UNIQUE(room_id, peer_id)
);

CREATE INDEX host_sessions_expiry_idx ON host_sessions(expires_at);

CREATE TABLE gameplay_sessions (
    gameplay_session_id UUID PRIMARY KEY,
    room_id UUID NOT NULL REFERENCES rooms(room_id) ON DELETE CASCADE,
    client_peer_id UUID NOT NULL REFERENCES peers(peer_id) ON DELETE CASCADE,
    host_session_id UUID NOT NULL REFERENCES host_sessions(host_session_id) ON DELETE CASCADE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_activity_at TIMESTAMPTZ NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL
);

CREATE INDEX gameplay_sessions_expiry_idx ON gameplay_sessions(expires_at);
CREATE INDEX gameplay_sessions_client_idx ON gameplay_sessions(client_peer_id);
