-- CI only (saves-roundtrip.yml): the API's migrations need PostgreSQL 18's built-in uuidv7().
-- Hosted Windows and macOS runners ship an older PostgreSQL, so this defines a compatible
-- public.uuidv7() there (48-bit Unix milliseconds, version 7, RFC 4122 variant, random rest).
-- It does nothing on PostgreSQL 18 or newer, and the production server never runs it.
DO $do$
BEGIN
  IF current_setting('server_version_num')::int < 180000 THEN
    EXECUTE $f$
      CREATE OR REPLACE FUNCTION public.uuidv7() RETURNS uuid AS $body$
      DECLARE
        ms bigint := floor(extract(epoch FROM clock_timestamp()) * 1000);
        b bytea := decode(replace(gen_random_uuid()::text, '-', ''), 'hex');
      BEGIN
        FOR i IN 0..5 LOOP
          b := set_byte(b, i, ((ms >> (40 - 8 * i)) & 255)::int);
        END LOOP;
        b := set_byte(b, 6, (get_byte(b, 6) & 15) | 112);
        b := set_byte(b, 8, (get_byte(b, 8) & 63) | 128);
        RETURN encode(b, 'hex')::uuid;
      END
      $body$ LANGUAGE plpgsql VOLATILE
    $f$;
  END IF;
END
$do$;
