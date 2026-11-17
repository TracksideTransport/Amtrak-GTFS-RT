"""Lead-time profiles adopt only observed stations that need a wider window."""

import json
from pathlib import Path

from worker.tracks.lead_time import SCHEMA, LeadProfile


def test_pvd_hot_window_is_tighter_and_sparse_stations_stay_global(tmp_path: Path):
    path = tmp_path / "stations.profile.json"
    path.write_text(
        json.dumps(
            {
                "schema": SCHEMA,
                "stations": [
                    {
                        "station": "PVD",
                        "confidence": "observed",
                        "compared_to_default": "wider",
                        "suggested_window_before_secs": 600,
                        "suggested_hot_window_secs": 180,
                    },
                    {
                        "station": "NYP",
                        "confidence": "sparse",
                        "compared_to_default": "wider",
                        "suggested_window_before_secs": 9000,
                        "suggested_hot_window_secs": 60,
                    },
                    {
                        "station": "BOS",
                        "confidence": "no_track",
                        "compared_to_default": "unknown",
                        "suggested_window_before_secs": 9000,
                        "suggested_hot_window_secs": 60,
                    },
                ],
            }
        ),
        encoding="utf-8",
    )
    profile = LeadProfile.load(str(path), default_before=1800, default_hot=720)
    assert profile.before_secs("PVD", 1800) == 600
    assert profile.hot_secs("pvd", 720) == 180
    assert profile.before_secs("NYP", 1800) == 1800
    assert profile.before_secs("ZZZ", 1800) == 1800
    assert LeadProfile.load(None, default_before=1800, default_hot=720).before_secs("PVD", 1800) == 1800


def test_covered_and_unrecognised_stations_keep_the_global_window(tmp_path: Path):
    """Ported from the deleted Rust copy: ``covered`` is not narrowed, and the hot window
    falls back to the global one when the profile omits it."""
    path = tmp_path / "stations.profile.json"
    path.write_text(
        json.dumps(
            {
                "schema": SCHEMA,
                "stations": [
                    {
                        "station": "WAS",
                        "confidence": "observed",
                        "compared_to_default": "covered",
                        "suggested_window_before_secs": 600,
                        "suggested_hot_window_secs": 60,
                    },
                    {
                        "station": "NHV",
                        "confidence": "observed",
                        "compared_to_default": "wider",
                        "suggested_window_before_secs": 2400,
                    },
                ],
            }
        ),
        encoding="utf-8",
    )
    profile = LeadProfile.load(str(path), default_before=1800, default_hot=720)
    assert profile.before_secs("WAS", 1800) == 1800
    assert profile.hot_secs("WAS", 720) == 720
    assert profile.before_secs("NHV", 1800) == 2400
    assert profile.hot_secs("NHV", 720) == 720


def test_a_profile_with_another_schema_is_ignored(tmp_path: Path):
    path = tmp_path / "other.json"
    path.write_text(json.dumps({"schema": "other", "stations": [
        {"station": "PVD", "confidence": "observed", "compared_to_default": "wider",
         "suggested_window_before_secs": 600},
    ]}), encoding="utf-8")
    profile = LeadProfile.load(str(path), default_before=1800, default_hot=720)
    assert profile.stations == {}
    assert profile.before_secs("PVD", 1800) == 1800
