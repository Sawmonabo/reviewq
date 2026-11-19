CREATE TABLE review_source_signals (
    review_id TEXT PRIMARY KEY REFERENCES reviews(review_id) ON DELETE CASCADE,
    settings_revision INTEGER NOT NULL CHECK (settings_revision >= 0),
    source_sha TEXT NOT NULL CHECK (length(source_sha) IN (40,64)),
    personal_approval INTEGER CHECK (personal_approval IN (0,1)),
    our_comments INTEGER NOT NULL CHECK (our_comments >= 0),
    observed_at_ms INTEGER NOT NULL,
    our_threads INTEGER CHECK (our_threads >= 0),
    open_threads INTEGER CHECK (open_threads BETWEEN 0 AND our_threads),
    CHECK (our_threads IS NOT NULL OR open_threads IS NULL)
) STRICT, WITHOUT ROWID;

CREATE TABLE review_discovery_due (
    review_id TEXT PRIMARY KEY REFERENCES review_discoveries(review_id) ON DELETE CASCADE,
    settings_revision INTEGER NOT NULL,
    source_sha TEXT NOT NULL,
    next_due_at_ms INTEGER NOT NULL
) STRICT, WITHOUT ROWID;

CREATE VIEW review_active_sessions AS
    SELECT s.*, c.seen_at_ms AS companion_at_ms FROM sessions s
    JOIN runs r ON r.run_id=s.run_id
    LEFT JOIN claude_companions c ON c.session_key=r.lead_session_key
    LEFT JOIN claude_children child ON child.session_key=s.session_key
    WHERE s.provider <> 'claude'
       OR (c.seen_at_ms > unixepoch('subsec') * 1000 - 30000 AND
           (s.parent_session_key IS NULL OR child.companion_generation=c.generation));

CREATE VIEW review_activity_evidence AS
    SELECT r.review_id, r.state='open' AS is_open,
      x.run_id, x.lead_session_key, lead.host_activity,
      coalesce(r.author_id=(SELECT CAST(identity_id AS INTEGER) FROM settings WHERE provider='gitlab')
         OR EXISTS (SELECT 1 FROM settings_reviewers u WHERE u.reviewer_id=r.author_id),0) AS own,
      s.personal_approval, coalesce(s.our_comments,0) AS our_comments,
      (SELECT count(*) FROM notifications n WHERE n.review_id=r.review_id AND n.source='gitlab'
         AND n.kind IN ('reply','edited_reply') AND n.read_at_ms IS NULL AND n.dismissed_at_ms IS NULL) AS unread_replies,
      EXISTS (SELECT 1 FROM review_active_sessions n JOIN runs nr USING(run_id)
         WHERE nr.review_id=r.review_id AND n.host_questions=1
          AND (nr.run_id=x.run_id OR EXISTS (SELECT 1 FROM notifications q
             WHERE q.session_key=n.session_key AND q.run_id=n.run_id AND q.kind='agent_question'
               AND q.occurred_at_ms>=x.started_at_ms))) AS has_questions,
      EXISTS (SELECT 1 FROM review_active_sessions n JOIN runs nr USING(run_id)
         WHERE nr.review_id=r.review_id AND n.host_questions=1 AND n.host_activity='blocked'
          AND (nr.run_id=x.run_id OR EXISTS (SELECT 1 FROM notifications q
             WHERE q.session_key=n.session_key AND q.run_id=n.run_id AND q.kind='agent_question'
               AND q.occurred_at_ms>=x.started_at_ms))) AS blocking_question,
      EXISTS (SELECT 1 FROM review_active_sessions n WHERE n.run_id=x.run_id
         AND n.host_activity='working') AS family_working,
      EXISTS (SELECT 1 FROM review_active_sessions n WHERE n.run_id=x.run_id
         AND n.host_activity='failed') AS family_failed,
      action.state AS action_state, action.intent AS action_intent,
      (SELECT h.state FROM report_handoffs h WHERE h.run_id=x.run_id AND h.reconciled_by_batch_id IS NULL
         ORDER BY h.created_at_ms DESC,h.receipt_id DESC LIMIT 1) AS report_state,
      (SELECT batch.state FROM followup_batches batch JOIN followup_monitors monitor USING(review_id)
         WHERE batch.review_id=r.review_id AND batch.generation=monitor.generation AND monitor.state='enabled'
           AND batch.authority_verified=1 AND batch.state IN ('pending','reserved','executing','reconciliation')
         ORDER BY CASE batch.state WHEN 'reconciliation' THEN 0 WHEN 'executing' THEN 1 ELSE 2 END,
                  batch.latest_observed_at_ms DESC,batch.batch_id DESC LIMIT 1) AS followup_state,
      EXISTS (SELECT 1 FROM messages m WHERE m.run_id=x.run_id AND m.state='uncertain') AS uncertain_delivery,
      (SELECT state FROM recovery_episodes k WHERE k.run_id=x.run_id AND k.lead_session_key=x.lead_session_key
          AND k.head_sha=r.head_sha AND k.settings_revision=(SELECT settings_revision FROM service_state WHERE singleton=1)
          ORDER BY k.observed_at_ms DESC,k.episode_id DESC LIMIT 1) AS recovery_state,
      EXISTS (SELECT 1 FROM notifications n WHERE n.review_id=r.review_id AND n.kind='approval_withdrawn'
         AND n.ordinal > coalesce(handled.source_ordinal,0)) AS approval_withdrawn,
      EXISTS (SELECT 1 FROM notifications n WHERE n.review_id=r.review_id AND n.source='gitlab'
         AND n.kind IN ('reply','edited_reply','reopened','resolved','approval_withdrawn')
         AND n.ordinal > coalesce(handled.source_ordinal,0)) AS pending_source,
      EXISTS (SELECT 1 FROM report_heads h JOIN report_versions v USING(run_id,receipt_id)
         WHERE h.run_id=x.run_id AND NOT EXISTS (SELECT 1 FROM review_handled_reports k
            WHERE k.review_id=r.review_id AND k.run_id=h.run_id AND k.attention_receipt_id=v.attention_receipt_id)) AS pending_report,
      CASE WHEN d.reviewed_sha=r.head_sha THEN d.disposition END AS current_decision,
      d.disposition, d.reviewed_sha,
      report.full_mr_reviewed_sha, report.coverage,
      coalesce(d.reviewed_at_ms IS NOT NULL OR report.full_mr_reviewed_sha IS NOT NULL,0) AS hide_reviewed,
      coalesce(d.reviewed_sha<>r.head_sha OR report.full_mr_reviewed_sha<>r.head_sha,
         d.reviewed_sha<>r.head_sha,report.full_mr_reviewed_sha<>r.head_sha,0) AS changed_after_handoff,
      coalesce((SELECT s.session_key FROM review_active_sessions s JOIN runs z USING(run_id)
          WHERE z.review_id=r.review_id
          AND (s.run_id=x.run_id OR (s.host_questions=1 AND EXISTS (
             SELECT 1 FROM notifications q WHERE q.run_id=s.run_id AND q.session_key=s.session_key
                AND q.kind='agent_question' AND q.occurred_at_ms>=x.started_at_ms)))
          AND (s.host_questions=1 OR s.host_activity IN ('blocked','working','failed'))
          ORDER BY CASE WHEN s.host_activity='blocked' AND s.host_questions=1 THEN 0
                        WHEN s.host_activity='blocked' THEN 1
                        WHEN s.session_key=x.lead_session_key AND s.host_activity='working' THEN 2
                        WHEN s.host_activity='working' THEN 3 WHEN s.host_questions=1 THEN 4 ELSE 5 END,
                   s.session_key LIMIT 1), x.lead_session_key) AS focus_session,
      max(coalesce(x.started_at_ms,0),coalesce(action.updated_at_ms,0),coalesce(report.captured_at_ms,0),
          coalesce((SELECT n.occurred_at_ms FROM notifications n WHERE n.review_id=r.review_id
             ORDER BY n.occurred_at_ms DESC LIMIT 1),0)) AS last_activity
    FROM reviews r
    LEFT JOIN runs x ON x.run_id=(SELECT run_id FROM runs WHERE review_id=r.review_id
         ORDER BY started_at_ms DESC,run_id DESC LIMIT 1)
    LEFT JOIN review_active_sessions lead ON lead.session_key=x.lead_session_key
    LEFT JOIN review_decisions d ON d.review_id=r.review_id
    LEFT JOIN review_handled_frontiers handled ON handled.review_id=r.review_id
    LEFT JOIN review_source_signals s ON s.review_id=r.review_id AND s.source_sha=r.head_sha
       AND s.settings_revision=(SELECT settings_revision FROM service_state WHERE singleton=1)
    LEFT JOIN report_heads h ON h.run_id=x.run_id
    LEFT JOIN report_versions report ON (report.run_id,report.receipt_id)=(h.run_id,h.receipt_id)
    LEFT JOIN action_heads ah ON ah.run_id=x.run_id
    LEFT JOIN actions action ON action.action_id=ah.action_id AND action.authority_verified=1 AND action.reviewed_sha=r.head_sha;

CREATE VIEW review_activity AS
    SELECT e.*, (SELECT run_id FROM sessions s WHERE s.session_key=e.focus_session) AS focus_run, reviewq_stage(
        is_open,run_id IS NOT NULL,host_activity,blocking_question,has_questions,family_working,family_failed,
        action_state,action_intent,report_state,followup_state,uncertain_delivery,personal_approval,
        approval_withdrawn,pending_source,pending_report,current_decision,our_comments>0,recovery_state
    ) AS stage_reason,
    reviewq_queue_state(is_open,personal_approval,pending_source,changed_after_handoff,current_decision,our_comments>0) AS queue_state
    FROM review_activity_evidence e;
