"""Assemble source files and versioned provider inputs."""

from .bundle import encode_json, index_files, record_extents
from .documents import RICH_REPORT, clipboard_sources, patch_documents, report_sources
from .gitlab import CHANGED_HEAD, discussion_id, gitlab_sources
from .jira import jira_records
from .native import native_inventory, registrations
from .presets import initial_source


PROFILES = ("populated", "measurement-active", "measurement-large")


def profile_records(rows, preset):
    profiles = {"populated": {"registrations": [row["alias"] for row in rows]}}
    if preset == "worked":
        for size in ("active", "large"):
            profiles[f"measurement-{size}"] = {
                "registrations": ["active"],
                "patch": {"review": "review-0", "source": f"../{size}.patch"},
            }
    return profiles


def bundle_files(preset):
    source_files, records, outbox = initial_source(preset)
    registered = registrations(records["sessions"])
    patches = patch_documents(preset) if records["reviews"] else {}
    reports, report_index = report_sources(records["reviews"], preset)
    profiles = profile_records(registered, preset)
    provider_files = {
        **patches, **reports, **native_inventory(registered, records["history"]),
        "gitlab.json": encode_json(gitlab_sources(records, preset)),
        "jira.json": encode_json(jira_records(records["reviews"], preset)),
        "presets.json": encode_json({"preset": preset, "profiles": profiles}),
        "registrations.json": encode_json(registered),
        "report-sources.json": encode_json(report_index),
        "timeline.json": encode_json(records["timeline"]),
        "outbox.json": encode_json(outbox),
    }
    expected = {
        "review_count": len(records["reviews"]), "registration_count": len(registered),
        "outbox_count": len(outbox), "first_review_keys": [
            [row["project_id"], row["iid"]] for row in records["reviews"][:3]
        ],
        "discussion_ids": {row["id"]: discussion_id(row["id"]) for row in records["discussions"]},
        "worked_patch_extents": {
            name: record_extents(source_files[name]) for name in ("quiet.patch", "active.patch", "large.patch")
            if name in source_files
        },
        "document_extents": {name: record_extents(data) for name, data in {**patches, **reports}.items()},
        "visual_specimens": {
            "wide": {"width": 1440, "height": 1000, "review": "review-0", "file": "src/example.ts"},
            "narrow": {"width": 420, "height": 900, "review": "review-0", "file": "src/example.ts"},
            "wrapped": {"width": 780, "height": 850, "review": "review-2", "file": "src/example-mixed.ts"},
            "reduced_motion": {"width": 1440, "height": 1000, "review": "review-2", "reduced_motion": "reduce"},
        } if records["reviews"] else {},
    }
    if preset in ("reader", "rich"):
        expected["reader"] = {
            "lead": "reader", "initial_items": 10000, "append_items": 100,
            "pinned_pages": 6, "page_items": 100, "first": "reader-item-0",
            "middle": "reader-item-5000", "last": "reader-item-9999", "after": "reader-item-10099",
            "long_output_bytes": 320 * 1024, "long_output_end": "READER-OUTPUT-TAIL",
        }
    if preset == "rich":
        expected.update({"clipboard": clipboard_sources(), "changed_head": CHANGED_HEAD,
                         "tail_report": RICH_REPORT})
    provider_files["manifest.json"] = encode_json({
        "version": 1, "preset": preset, "files": index_files(provider_files), "expected": expected,
    })
    source_files.update({f"provider/{name}": data for name, data in provider_files.items()})
    return source_files
