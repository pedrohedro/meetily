"""Self-check for meetily_mcp data functions against a throwaway database.

Run: python3 -I test_meetily_mcp.py   (no MCP dependency needed)
"""

import json
import os
import sqlite3
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))


def make_db(path: Path) -> None:
    conn = sqlite3.connect(path)
    conn.executescript(
        """
        CREATE TABLE meetings (id TEXT PRIMARY KEY, title TEXT, created_at TEXT, updated_at TEXT, folder_path TEXT);
        CREATE TABLE transcripts (id TEXT PRIMARY KEY, meeting_id TEXT, transcript TEXT, timestamp TEXT,
            summary TEXT, action_items TEXT, key_points TEXT, audio_start_time REAL, audio_end_time REAL,
            duration REAL, speaker TEXT);
        CREATE TABLE summary_processes (meeting_id TEXT PRIMARY KEY, status TEXT, created_at TEXT,
            updated_at TEXT, error TEXT, result TEXT);
        """
    )
    conn.executemany(
        "INSERT INTO meetings VALUES (?,?,?,?,NULL)",
        [("m1", "Old sync", "2026-11-01T10:00:00", ""), ("m2", "Teams planning", "2026-11-18T10:00:00", "")],
    )
    conn.executemany(
        "INSERT INTO transcripts (id, meeting_id, transcript, timestamp, audio_start_time, audio_end_time, speaker) VALUES (?,?,?,?,?,?,?)",
        [
            ("t2", "m2", "eu vou mandar o 100%_ relatório", "10:01", 75.0, 80.0, ""),
            ("t1", "m2", "bom dia", "10:00", 5.0, 9.5, ""),
            ("t3", "m1", "nada relevante", "10:00", 1.0, 2.0, None),
        ],
    )
    conn.executemany(
        "INSERT INTO summary_processes (meeting_id, status, created_at, updated_at, result) VALUES (?,?,?,?,?)",
        [
            ("m2", "completed", "", "", json.dumps({"markdown": "# Planning\n- decide X"})),
            ("m1", "completed", "", "", json.dumps({"english_cache": {"markdown": "# Old"}})),
        ],
    )
    conn.commit()
    conn.close()


def main() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        db = Path(tmp) / "meeting minutes.sqlite"  # space on purpose, like "Application Support"
        make_db(db)
        os.environ["MEETILY_DB"] = str(db)
        import meetily_mcp as m

        listed = m.list_meetings()
        assert [x["id"] for x in listed] == ["m2", "m1"], listed
        assert listed[0]["segments"] == 2 and listed[0]["duration_seconds"] == 80.0
        assert [x["id"] for x in m.list_meetings(since="2026-11-10")] == ["m2"]

        meeting = m.get_meeting("m2")
        assert meeting["summary_markdown"].startswith("# Planning"), meeting
        assert meeting["created_at_local"].startswith("2026-11-18 "), meeting  # naive ISO read as local
        assert m.get_meeting("m1")["summary_markdown"] == "# Old"  # english_cache fallback
        assert m.get_latest_meeting()["id"] == "m2"

        tr = m.get_meeting_transcript("m2", limit=1)
        assert tr["lines"] == ["[00:05] bom dia"] and tr["next_offset"] == 1 and tr["total_segments"] == 2, tr
        tr2 = m.get_meeting_transcript("m2", offset=1)
        assert tr2["lines"] == ["[01:15] eu vou mandar o 100%_ relatório"] and tr2["next_offset"] is None
        assert tr2["speakers_available"] is False

        hits = m.search_transcripts("100%_")  # LIKE wildcards must be literal
        assert [h["meeting_id"] for h in hits] == ["m2"], hits
        assert [h["meeting_id"] for h in m.search_transcripts("%")] == ["m2"]  # literal %, not match-all

        try:
            m.get_meeting("nope")
        except ValueError:
            pass
        else:
            raise AssertionError("missing meeting should raise")

        # read-only: writes through the module's connection must fail
        with m.connect() as conn:
            try:
                conn.execute("DELETE FROM meetings")
            except sqlite3.OperationalError as e:
                assert "readonly" in str(e), e
            else:
                raise AssertionError("connection is not read-only")
    print("ok")


if __name__ == "__main__":
    main()
