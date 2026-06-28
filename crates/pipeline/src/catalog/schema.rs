pub const MIGRATIONS: &[&str] = &[
    // version 1 — initial schema
    "BEGIN TRANSACTION;
     INSERT INTO schema_version VALUES (1);
     CREATE SEQUENCE seq_files_id START 1;
     CREATE TABLE files (
         id              BIGINT PRIMARY KEY DEFAULT nextval('seq_files_id'),
         path            VARCHAR NOT NULL UNIQUE,
         content_hash    VARCHAR NOT NULL,
         size_bytes      BIGINT NOT NULL,
         mtime_ns        BIGINT NOT NULL,
         file_format     VARCHAR NOT NULL,
         has_sidecar_jpg BOOLEAN NOT NULL DEFAULT false,
         last_processed  BIGINT NOT NULL
     );
     CREATE INDEX idx_files_hash ON files(content_hash);
     CREATE TABLE exif (
         file_id              BIGINT PRIMARY KEY REFERENCES files(id),
         captured_at          BIGINT,
         camera_make          VARCHAR,
         camera_model         VARCHAR,
         lens_model           VARCHAR,
         focal_length_mm      REAL,
         aperture             REAL,
         iso                  INTEGER,
         shutter_seconds      REAL,
         width                INTEGER,
         height               INTEGER,
         orientation          SMALLINT
     );
     CREATE INDEX idx_exif_captured ON exif(captured_at);
     CREATE INDEX idx_exif_lens ON exif(camera_model, lens_model);
     CREATE TABLE sharpness (
         file_id          BIGINT PRIMARY KEY REFERENCES files(id),
         s_global         REAL NOT NULL,
         s_subject        REAL,
         s_background     REAL,
         subject_ratio    REAL,
         detector_used    VARCHAR
     );
     CREATE TABLE exposure (
         file_id              BIGINT PRIMARY KEY REFERENCES files(id),
         clipped_highlights   REAL NOT NULL,
         clipped_shadows      REAL NOT NULL,
         mean_luma            REAL NOT NULL,
         histogram_skew       REAL NOT NULL
     );
     CREATE TABLE iqa (
         file_id     BIGINT PRIMARY KEY REFERENCES files(id),
         model       VARCHAR NOT NULL,
         score       REAL NOT NULL
     );
     CREATE TABLE embeddings (
         file_id     BIGINT PRIMARY KEY REFERENCES files(id),
         model       VARCHAR NOT NULL,
         vector      FLOAT[]  NOT NULL
     );
     CREATE SEQUENCE seq_defect_flags_id START 1;
     CREATE TABLE defect_flags (
         id              BIGINT PRIMARY KEY DEFAULT nextval('seq_defect_flags_id'),
         file_id         BIGINT NOT NULL REFERENCES files(id),
         flag_type       VARCHAR NOT NULL,
         confidence      REAL NOT NULL,
         reason          VARCHAR,
         UNIQUE(file_id, flag_type)
     );
     CREATE INDEX idx_flags_type ON defect_flags(flag_type);
     CREATE SEQUENCE seq_dup_groups_id START 1;
     CREATE TABLE duplicate_groups (
         id              BIGINT PRIMARY KEY DEFAULT nextval('seq_dup_groups_id'),
         method          VARCHAR NOT NULL,
         created_at      BIGINT NOT NULL
     );
     CREATE TABLE duplicate_members (
         group_id            BIGINT NOT NULL REFERENCES duplicate_groups(id),
         file_id             BIGINT NOT NULL REFERENCES files(id),
         is_suggested_keeper BOOLEAN NOT NULL DEFAULT false,
         quality_score       REAL,
         PRIMARY KEY (group_id, file_id)
     );
     CREATE INDEX idx_dup_members_file ON duplicate_members(file_id);
     CREATE TABLE sharpness_baseline (
         camera_model     VARCHAR NOT NULL,
         lens_model       VARCHAR NOT NULL,
         focal_bucket     INTEGER NOT NULL,
         aperture_bucket  REAL NOT NULL,
         s_subject_p10    REAL NOT NULL,
         s_subject_p50    REAL NOT NULL,
         s_subject_p90    REAL NOT NULL,
         n_samples        INTEGER NOT NULL,
         last_updated     BIGINT NOT NULL,
         PRIMARY KEY (camera_model, lens_model, focal_bucket, aperture_bucket)
     );
     COMMIT;",
    // version 2 — review decisions
    "BEGIN TRANSACTION;
     INSERT INTO schema_version VALUES (2);
     CREATE TABLE decisions (
         file_id      BIGINT PRIMARY KEY REFERENCES files(id),
         verdict      VARCHAR NOT NULL,
         is_keeper    BOOLEAN NOT NULL DEFAULT false,
         note         VARCHAR,
         decided_at   BIGINT NOT NULL
     );
     CREATE INDEX idx_decisions_verdict ON decisions(verdict);
     COMMIT;",
    // version 3 — per-folder library identity
    "BEGIN TRANSACTION;
     INSERT INTO schema_version VALUES (3);
     CREATE TABLE library_meta (
         folder_path   VARCHAR NOT NULL,
         created_at    BIGINT  NOT NULL,
         last_analyzed BIGINT
     );
     COMMIT;",
];
