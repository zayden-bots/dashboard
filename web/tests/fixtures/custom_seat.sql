INSERT INTO guilds (id)
    VALUES (7);

INSERT INTO applications (application_id, slot, kind, bot_user_id, name, public_key, token_ct, token_nonce, key_id, registered_by)
    VALUES (2000, 'zayden', 'custom', 2001, 'Custom', 'key', '\x00'::bytea, '\x00'::bytea, 1, 1);

INSERT INTO application_guilds (guild_id, slot, application_id, attached_by)
    VALUES (7, 'zayden', 2000, 1);

INSERT INTO guild_presence (guild_id, application_id)
    VALUES (7, 2000);

INSERT INTO entitlements (provider, external_id, scope_type, scope_id, tier)
    VALUES ('test', 'guild-7', 'guild', 7, 'ultra');
