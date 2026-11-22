-- The index of text pieces was built after the table was last analysed, so the
-- planner had no idea how rare a search term is. It read messages newest
-- first and tested each one: two to eight seconds for a rare word over two
-- years, where the index answers in milliseconds.
ANALYZE crm.message_search;
