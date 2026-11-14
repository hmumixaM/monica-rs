-- A missing source role is evidence of uncertainty, never an ordinary member.
ALTER TABLE crm.source_group_memberships ALTER COLUMN role DROP NOT NULL;
