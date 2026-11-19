use rusqlite::{Connection, params};

use crate::StoreError;

/// An accepted result settles only exact predecessor observations proven by its
/// selected membership. Earlier groups remain separately receipted.
pub(super) fn accept(db: &Connection, review: &str, current: &str) -> Result<(), StoreError> {
    db.execute(
        "WITH RECURSIVE
         exact_edges AS NOT MATERIALIZED (
             SELECT p.batch_id,p.observation_id,p.predecessor_batch_id,p.predecessor_observation_id
             FROM followup_member_predecessors p
             JOIN followup_observations a ON a.review_id=p.review_id AND a.observation_id=p.observation_id
             JOIN followup_observations z ON z.review_id=p.review_id AND z.observation_id=p.predecessor_observation_id
             WHERE p.review_id=?1 AND a.discussion_id=z.discussion_id
               AND a.discussion_version=z.discussion_version AND a.occurrence_key=z.occurrence_key
               AND NOT EXISTS (
                   SELECT 1 FROM followup_observation_origins earlier
                   WHERE earlier.review_id=p.review_id AND earlier.observation_id=z.observation_id
                     AND NOT EXISTS (
                         SELECT 1 FROM followup_observation_origins later
                          WHERE later.review_id=earlier.review_id AND later.observation_id=a.observation_id
                            AND later.original_note_id=earlier.original_note_id
                            AND NOT EXISTS (
                                SELECT 1 FROM followup_thread_findings f
                                 WHERE f.review_id=earlier.review_id AND f.generation=earlier.generation
                                   AND f.original_note_id=earlier.original_note_id AND NOT EXISTS (
                                    SELECT 1 FROM followup_thread_findings n
                                     WHERE n.review_id=later.review_id AND n.generation=later.generation
                                       AND n.original_note_id=later.original_note_id AND n.finding_id=f.finding_id)
                            )
                     )
               )
         ),
         ancestors(batch_id,observation_id) AS (
             SELECT predecessor_batch_id,predecessor_observation_id FROM exact_edges WHERE batch_id=?2
             UNION
             SELECT p.predecessor_batch_id,p.predecessor_observation_id FROM ancestors a
              JOIN exact_edges p USING(batch_id,observation_id)
         )
         INSERT INTO followup_reconciled_observations(review_id,batch_id,observation_id,assessment_batch_id)
         SELECT ?1,batch_id,observation_id,?2 FROM ancestors WHERE true ON CONFLICT DO NOTHING",
        params![review,current],
    )?;
    db.execute(
        "UPDATE report_handoffs AS h SET reconciled_by_batch_id=?2
         WHERE h.state='failed' AND h.artifact_id IS NOT NULL AND h.reconciled_by_batch_id IS NULL
           AND h.batch_id IN (SELECT batch_id FROM followup_reconciled_observations WHERE review_id=?1 AND assessment_batch_id=?2)
           AND NOT EXISTS (SELECT 1 FROM followup_members m WHERE m.review_id=?1 AND m.batch_id=h.batch_id
               AND NOT EXISTS (SELECT 1 FROM followup_reconciled_observations a
                   WHERE a.review_id=m.review_id AND a.batch_id=m.batch_id AND a.observation_id=m.observation_id))",
        params![review,current],
    )?;
    Ok(())
}
