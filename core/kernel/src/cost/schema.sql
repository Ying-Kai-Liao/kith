
      create table if not exists req (id text primary key, session text, ts text, project text, model text,
        agent text, turn text, input int, output int, cache_read int, cache_write int, cost real);
      create table if not exists seen (path text primary key, size int);
      create index if not exists req_ts on req(ts);
      create index if not exists req_session on req(session);
      create table if not exists runs (session text primary key, agent text, version text, model text, effort text,
        via text, project text, started text);