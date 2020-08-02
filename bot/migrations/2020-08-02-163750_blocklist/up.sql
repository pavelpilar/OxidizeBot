CREATE TABLE blocked_songs (
    track_id VARCHAR NOT NULL,
    blocked_by VARCHAR NOT NULL,
    blocked_at TIMESTAMP NOT NULL,

    PRIMARY KEY (track_id)
);
