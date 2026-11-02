//! Fresh v2 schema. Child ownership is maintained explicitly in transactions:
//! no foreign-key PRAGMA, cascade or trigger behavior is assumed of the engine.
pub(super) const VERSION: i64 = 2;
pub(super) const SCHEMA: &str = "
CREATE TABLE notifications (
 history_id INTEGER PRIMARY KEY AUTOINCREMENT,
 session_id INTEGER NOT NULL CHECK(session_id BETWEEN 1 AND 4294967295),
 app_key TEXT NOT NULL,
 app_name TEXT NOT NULL,
 app_icon TEXT NOT NULL,
 identity_icon TEXT NOT NULL,
 summary TEXT NOT NULL,
 body TEXT NOT NULL,
 urgency INTEGER NOT NULL CHECK(urgency BETWEEN 0 AND 255),
 category TEXT NOT NULL,
 desktop_entry TEXT NOT NULL,
 image_path TEXT NOT NULL,
 sound_name TEXT NOT NULL,
 sound_file TEXT NOT NULL,
 resident INTEGER NOT NULL CHECK(resident IN (0,1)),
 suppress_sound INTEGER NOT NULL CHECK(suppress_sound IN (0,1)),
 image_data_present INTEGER NOT NULL CHECK(image_data_present IN (0,1)),
 created_unix_ms INTEGER NOT NULL CHECK(created_unix_ms >= 0),
 updated_unix_ms INTEGER NOT NULL CHECK(updated_unix_ms >= 0),
 expires_unix_ms INTEGER CHECK(expires_unix_ms >= 0),
 group_key TEXT NOT NULL,
 source_monitor TEXT NOT NULL,
 snoozed_until_unix_ms INTEGER CHECK(snoozed_until_unix_ms >= 0),
 dnd_bypass INTEGER NOT NULL CHECK(dnd_bypass IN (0,1)),
 closed_unix_ms INTEGER CHECK(closed_unix_ms >= 0),
 close_reason INTEGER CHECK(close_reason BETWEEN 0 AND 4294967295),
 search_text TEXT NOT NULL
) STRICT;
CREATE UNIQUE INDEX notifications_active_session ON notifications(session_id) WHERE closed_unix_ms IS NULL;
CREATE INDEX notifications_app ON notifications(app_key);
CREATE INDEX notifications_identity ON notifications(session_id,created_unix_ms);
CREATE INDEX notifications_age ON notifications(created_unix_ms);
CREATE TABLE notification_actions (
 history_id INTEGER NOT NULL,
 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
 action_key TEXT NOT NULL,
 label TEXT NOT NULL,
 PRIMARY KEY(history_id, ordinal)
) STRICT;
CREATE TABLE notification_context_actions (
 history_id INTEGER NOT NULL,
 ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
 action_key TEXT NOT NULL,
 label TEXT NOT NULL,
 icon TEXT NOT NULL,
 PRIMARY KEY(history_id, ordinal)
) STRICT;
CREATE TABLE notification_previews (
 history_id INTEGER PRIMARY KEY,
 app_name TEXT NOT NULL,
 app_icon TEXT NOT NULL,
 identity_icon TEXT NOT NULL,
 desktop_entry TEXT NOT NULL,
 image_path TEXT NOT NULL,
 summary TEXT NOT NULL,
 body TEXT NOT NULL,
 group_key TEXT NOT NULL,
 repeat_category TEXT,
 repeat_urgency INTEGER CHECK(repeat_urgency BETWEEN 0 AND 255),
 CHECK((repeat_category IS NULL) = (repeat_urgency IS NULL))
) STRICT;
CREATE TABLE notification_settings (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 countdown INTEGER NOT NULL CHECK(countdown IN (0,1)),
 code_expiry_seconds INTEGER NOT NULL CHECK(code_expiry_seconds=0 OR code_expiry_seconds BETWEEN 10 AND 300),
 history_days INTEGER NOT NULL CHECK(history_days BETWEEN 1 AND 30)
) STRICT;
INSERT INTO notification_settings VALUES (1,1,0,30);
CREATE TABLE notification_state (
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 last_notification_id INTEGER NOT NULL CHECK(last_notification_id BETWEEN 0 AND 4294967295),
 dnd INTEGER NOT NULL CHECK(dnd IN (0,1)),
 dnd_until_unix_ms INTEGER CHECK(dnd_until_unix_ms >= 0),
 CHECK(dnd=1 OR dnd_until_unix_ms IS NULL)
) STRICT;
INSERT INTO notification_state VALUES (1,0,0,NULL);
CREATE TABLE notification_app_policies (
 app_key TEXT PRIMARY KEY,
 silent INTEGER NOT NULL CHECK(silent IN (0,1)),
 until_unix_ms INTEGER CHECK(until_unix_ms >= 0),
 group_similar INTEGER NOT NULL CHECK(group_similar IN (0,1)),
 bypass_dnd INTEGER NOT NULL CHECK(bypass_dnd IN (0,1))
) STRICT;
CREATE TABLE notification_once(event_key TEXT PRIMARY KEY, created_unix_ms INTEGER NOT NULL) STRICT;
";
