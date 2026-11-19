WITH
requested_boards AS (SELECT value AS id FROM json_each(:boards)),
requested_projects AS (SELECT value AS id FROM json_each(:projects)),
requested_sprints AS (
    SELECT json_extract(value,'$[0]') AS board, json_extract(value,'$[1]') AS sprint
    FROM json_each(:pairs)
),
boards AS (
    SELECT b.* FROM settings_boards b
    WHERE b.enabled=1 AND (NOT EXISTS (SELECT 1 FROM requested_boards)
       OR b.board_id IN (SELECT id FROM requested_boards))
       AND (NOT EXISTS (SELECT 1 FROM requested_projects)
       OR EXISTS (SELECT 1 FROM settings_repositories p
           WHERE p.project_id IN (SELECT id FROM requested_projects)
             AND (p.board_selection='any' OR p.project_id IN (SELECT value FROM json_each(b.projects)))))
),
tickets AS (
    SELECT i.* FROM scoped_issues i JOIN boards b USING(board_id)
    WHERE :sprint='any'
       OR (:sprint='active' AND NOT EXISTS (SELECT 1 FROM boards WHERE kind='scrum'))
       OR (b.kind='scrum' AND (
            (:sprint='none' AND NOT EXISTS (
               SELECT 1 FROM scoped_issue_sprints m WHERE (m.board_id,m.ticket_key)=(i.board_id,i.ticket_key)))
         OR (:sprint='active' AND EXISTS (
               SELECT 1 FROM scoped_issue_sprints m JOIN scoped_sprints sprint USING(board_id,sprint_id)
               WHERE (m.board_id,m.ticket_key)=(i.board_id,i.ticket_key) AND sprint.active=1))
         OR (:sprint='selected' AND EXISTS (
               SELECT 1 FROM scoped_issue_sprints m JOIN requested_sprints s ON (s.board,s.sprint)=(m.board_id,m.sprint_id)
               WHERE (m.board_id,m.ticket_key)=(i.board_id,i.ticket_key)
               UNION ALL
               SELECT 1 FROM loaded_sprint_issues m JOIN requested_sprints s ON (s.board,s.sprint)=(m.board_id,m.sprint_id)
               WHERE (m.board_id,m.ticket_key)=(i.board_id,i.ticket_key)
                 AND NOT EXISTS (SELECT 1 FROM scoped_sprints x WHERE (x.board_id,x.sprint_id)=(m.board_id,m.sprint_id))))
       ))
),
matched AS (
    SELECT a.review_id,a.ticket_key FROM scoped_review_issues a JOIN tickets t USING(board_id,ticket_key)
),
base AS (
    SELECT r.review_id,r.author_id,r.author_username,r.updated_at_ms,
      f.own,f.stage_reason,f.queue_state,f.hide_reviewed,f.run_id,
      CASE WHEN f.stage_reason LIKE 'working/%' OR f.stage_reason LIKE 'needs_input/%' THEN 1 ELSE 0 END AS hide_in_progress,
      (SELECT m.ticket_key FROM matched m WHERE m.review_id=r.review_id
         ORDER BY reviewq_fold(substr(m.ticket_key,1,instr(m.ticket_key,'-')-1)),
                  length(m.ticket_key)-instr(m.ticket_key,'-'),m.ticket_key LIMIT 1) AS key
    FROM reviews r JOIN queue_projection f USING(review_id)
    WHERE (__SCOPE__) AND :empty_scope=0
      AND (NOT EXISTS (SELECT 1 FROM requested_projects) OR r.project_id IN (SELECT id FROM requested_projects))
      AND (EXISTS(SELECT 1 FROM matched m WHERE m.review_id=r.review_id)
           OR (:scope_code=2)
           OR (:default_scope=1 AND :sprint IN ('any','active') AND (
              (NOT EXISTS(SELECT 1 FROM settings_boards) AND
               EXISTS(SELECT 1 FROM review_discoveries d WHERE d.review_id=r.review_id))
              OR (:phase<>'archive'
                  AND (:sprint='any' OR NOT EXISTS(SELECT 1 FROM boards WHERE kind='scrum'))
                  AND EXISTS(SELECT 1 FROM review_tracking s WHERE s.review_id=r.review_id)
                  AND EXISTS(SELECT 1 FROM configured_projects p WHERE (p.origin,p.project_id)=(r.origin,r.project_id)))
           )))
      AND (__MR_SEARCH__)
),
mr_rows AS (
    SELECT m.review_id AS id,0 AS kind,m.key,m.updated_at_ms,m.author_username,
       NULL AS board_id,m.hide_reviewed,m.hide_in_progress
    FROM base m
    WHERE (:owner='all' OR (:owner='mine' AND own=1) OR (:owner='others' AND own=0))
      AND (:author IS NULL OR m.author_id=:author)
      AND ((:phase='all_open' AND m.queue_state<>'archive')
        OR (:phase='needs_review' AND m.queue_state='needs_review' AND m.run_id IS NULL AND m.hide_reviewed=0)
        OR (:phase NOT IN ('all_open','needs_review') AND m.queue_state=:phase))
),
unmatched AS (
    SELECT i.ticket_key AS id,1 AS kind,i.ticket_key AS key,0 AS updated_at_ms,NULL AS author_username,
       min(i.board_id) AS board_id,0 AS hide_reviewed,0 AS hide_in_progress
    FROM tickets i
    WHERE :scope_code=0 AND :empty_scope=0 AND :owner<>'mine' AND :author IS NULL
      AND :phase IN ('all_open','needs_review')
      AND (:search='' OR instr(reviewq_fold(i.ticket_key),:search)>0 OR instr(reviewq_fold(i.title),:search)>0
                      OR instr(reviewq_fold(i.status),:search)>0 OR instr(reviewq_fold(coalesce(i.assignee,'')),:search)>0
                      OR instr(reviewq_fold(coalesce(i.priority,'')),:search)>0)
      AND NOT EXISTS (
         SELECT 1 FROM matched m JOIN reviews r ON r.review_id=m.review_id
         WHERE m.ticket_key=i.ticket_key AND
           (NOT EXISTS (SELECT 1 FROM requested_projects) OR r.project_id IN (SELECT id FROM requested_projects)))
    GROUP BY i.ticket_key
),
candidates AS (
    SELECT * FROM mr_rows UNION ALL SELECT * FROM unmatched
),
visible AS (
    SELECT *,
      CASE WHEN :order='ticket' THEN (key IS NULL) ELSE kind END AS sort_group,
      CASE :order WHEN 'recent' THEN -updated_at_ms WHEN 'oldest' THEN updated_at_ms ELSE 0 END AS sort_time,
      CASE WHEN :order='author' AND kind=0 THEN reviewq_fold(coalesce(author_username,''))
           WHEN (:order='ticket' OR kind=1) AND key IS NOT NULL
             THEN reviewq_fold(substr(key,1,instr(key,'-')-1)) ELSE '' END AS sort_label,
      CASE WHEN (:order='ticket' OR kind=1) AND key IS NOT NULL
           THEN length(key)-instr(key,'-') ELSE 0 END AS sort_digits,
      CASE WHEN (:order='ticket' OR kind=1) AND key IS NOT NULL
           THEN substr(key,instr(key,'-')+1) ELSE '' END AS sort_number
    FROM candidates
    WHERE (:hide_reviewed=0 OR hide_reviewed=0) AND (:hide_in_progress=0 OR hide_in_progress=0)
)
