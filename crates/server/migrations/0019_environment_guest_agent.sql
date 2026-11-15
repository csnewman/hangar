-- The guest agent a running environment's machine has, as its worker
-- reports it: its protocol, its build, and whether it is other than the
-- worker's, which the environment takes when it next boots.
ALTER TABLE environments ADD COLUMN guest_agent jsonb;
