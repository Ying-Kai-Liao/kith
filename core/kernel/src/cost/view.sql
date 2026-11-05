
      drop view if exists hq_req;
      create view hq_req as
        select req.*, case when req.agent like 'kith:%' then substr(req.agent, 6, instr(substr(req.agent, 6) || ':', ':') - 1)
                           when req.agent like 'hq:%' then substr(req.agent, 4, instr(substr(req.agent, 4) || ':', ':') - 1)
                           else runs.agent end as hq_agent,
               case when req.agent like 'kith:%' or req.agent like 'hq:%' then null else runs.version end as version,
               case when req.agent like 'kith:%' or req.agent like 'hq:%' then 'subagent' else runs.via end as via,
               case when req.agent like 'kith:%' or req.agent like 'hq:%' then null else runs.effort end as effort,
               runs.person as person
        from req left join runs using (session);