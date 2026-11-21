-- Only bounded, live, non-private conversation text; no tool/system output.
CREATE VIRTUAL TABLE chat_messages_fts USING fts5(
    content, tokenize = 'unicode61 remove_diacritics 2'
);
CREATE VIEW chat_searchable_messages AS
SELECT rowid, replace(replace(CASE WHEN instr(content, char(10) || char(10) || '[[praxi-attachments:') > 0
    THEN substr(content, 1, instr(content, char(10) || char(10) || '[[praxi-attachments:') - 1)
    ELSE content END, 'ı', 'i'), 'İ', 'I') AS content
FROM messages WHERE role IN ('user', 'assistant') AND exposure_private = 0
    AND superseded = 0 AND length(CAST(content AS BLOB)) <= 65536;
INSERT INTO chat_messages_fts(rowid, content) SELECT rowid, content FROM chat_searchable_messages;
CREATE TRIGGER chat_search_insert AFTER INSERT ON messages BEGIN
    INSERT INTO chat_messages_fts(rowid, content)
    SELECT rowid, content FROM chat_searchable_messages WHERE rowid = new.rowid;
END;
CREATE TRIGGER chat_search_delete AFTER DELETE ON messages BEGIN
    DELETE FROM chat_messages_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER chat_search_update AFTER UPDATE ON messages BEGIN
    DELETE FROM chat_messages_fts WHERE rowid = old.rowid;
    INSERT INTO chat_messages_fts(rowid, content)
    SELECT rowid, content FROM chat_searchable_messages WHERE rowid = new.rowid;
END;
