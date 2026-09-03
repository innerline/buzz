-- Deletion tombstones for workflows (2026-09-03).
--
-- `workflows delete` (NIP-09 kind:5 a-tag deletion) removes the `workflows`
-- row, but any later kind:30620 definition event for the same coordinate —
-- e.g. `workflows update` — re-upserted the row, silently resurrecting the
-- deleted workflow. This table records each successful deletion so the def
-- ingest path can reject later definition events for the same workflow.
--
-- Recreating a workflow is still possible: `workflows create` generates a
-- fresh UUID, which no tombstone covers.
CREATE TABLE workflow_deletion_tombstones (
    community_id uuid NOT NULL REFERENCES communities(id) ON DELETE CASCADE,
    workflow_id uuid NOT NULL,
    owner_pubkey bytea NOT NULL,
    deleted_by bytea NOT NULL,
    deleted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (community_id, workflow_id)
);

-- Owner-scoped lookup for diagnostics and owner-console tooling.
CREATE INDEX idx_workflow_deletion_tombstones_owner
    ON workflow_deletion_tombstones (community_id, owner_pubkey);
