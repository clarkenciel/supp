-- Leaving keeps the membership row, marked left, so a document that has
-- ever had members is never claimable again (e.g. by another device of a
-- user who left it).
ALTER TABLE list_members ADD COLUMN left_at INTEGER;
