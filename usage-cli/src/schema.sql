CREATE TABLE meta(key TEXT PRIMARY KEY,value INTEGER NOT NULL);
INSERT INTO meta VALUES('generation',0);
CREATE TABLE sources(id TEXT PRIMARY KEY,format TEXT NOT NULL,path TEXT NOT NULL,digest TEXT NOT NULL,state TEXT NOT NULL,diagnostics TEXT NOT NULL,observed_at TEXT NOT NULL);
CREATE TABLE sessions(source_id TEXT NOT NULL REFERENCES sources(id),id TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(source_id,id));
CREATE TABLE calls(source_id TEXT NOT NULL REFERENCES sources(id),id TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(source_id,id));
CREATE TABLE turns(source_id TEXT NOT NULL REFERENCES sources(id),id TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(source_id,id));
CREATE TABLE requests(source_id TEXT NOT NULL REFERENCES sources(id),id TEXT NOT NULL,data TEXT NOT NULL,PRIMARY KEY(source_id,id));
CREATE INDEX calls_identity ON calls(id);
CREATE VIEW request_facts AS SELECT id,source_id,data FROM (
 SELECT id,source_id,data,row_number() OVER(PARTITION BY id ORDER BY source_id) AS choice FROM requests
) WHERE choice=1;
CREATE VIEW turn_facts AS SELECT id,source_id,data FROM (
 SELECT id,source_id,data,row_number() OVER(PARTITION BY id ORDER BY source_id) AS choice FROM turns
) WHERE choice=1;
CREATE VIEW native_aliases AS SELECT json_extract(data,'$.native_id') AS native_id,
 min(id) AS id,min(json_extract(data,'$.adapter')) AS adapter FROM sessions
 WHERE json_extract(data,'$.louiselm_origin')=1
 GROUP BY json_extract(data,'$.native_id') HAVING count(DISTINCT id)=1;
CREATE VIEW call_observations AS SELECT
 CASE WHEN a.id IS NULL THEN c.id ELSE a.id || substr(c.id,length(json_extract(c.data,'$.session_id'))+1) END AS id,
 c.source_id,
 CASE WHEN a.id IS NULL THEN c.data ELSE json_set(c.data,
 '$.id',a.id || substr(c.id,length(json_extract(c.data,'$.session_id'))+1),
 '$.session_id',a.id,'$.adapter',a.adapter) END AS data
 FROM calls c LEFT JOIN native_aliases a ON json_extract(c.data,'$.adapter')='acp'
 AND a.native_id=json_extract(c.data,'$.native_session_id');
CREATE VIEW base_calls AS
 WITH observations AS MATERIALIZED (SELECT * FROM call_observations),
 identities AS MATERIALIZED (
  SELECT id,count(*) AS observation_count,json_group_array(DISTINCT source_id) AS source_ids,
   CASE WHEN count(DISTINCT json_extract(data,'$.rpc_request_id'))=1 THEN min(json_extract(data,'$.rpc_request_id')) END AS rpc_request_id,
   count(DISTINCT json_extract(data,'$.command_key'))>1 OR count(DISTINCT json_extract(data,'$.output_fingerprint'))>1 AS conflict
  FROM observations GROUP BY id
 ), ranked AS (
  SELECT c.*,row_number() OVER(PARTITION BY c.id ORDER BY CASE WHEN s.format='acp' THEN 1 ELSE 0 END,c.source_id) AS choice
  FROM observations c JOIN sources s ON s.id=c.source_id
 )
 SELECT r.id,r.source_id,json_set(r.data,'$.source_ids',json(i.source_ids),'$.observation_count',i.observation_count,'$.conflicting_observations',json(CASE WHEN i.conflict THEN 'true' ELSE 'false' END)) AS data,i.rpc_request_id
 FROM ranked r JOIN identities i ON i.id=r.id WHERE r.choice=1;
CREATE VIEW derived_calls AS
 WITH durable AS MATERIALIZED (
  SELECT min(data) AS data,min(id) AS id,json_extract(data,'$.native_session_id') AS native_session,
   json_extract(data,'$.rpc_request_id') AS rpc
  FROM turn_facts WHERE json_extract(data,'$.dispatched')=1
  GROUP BY native_session,rpc HAVING count(*)=1 AND rpc IS NOT NULL
 ), native_sessions AS MATERIALIZED (
  SELECT DISTINCT json_extract(data,'$.native_id') AS native_id FROM sessions
  WHERE json_extract(data,'$.adapter') NOT IN ('acp','louiselm')
 )
 SELECT c.id,c.source_id,json_set(
  CASE WHEN t.id IS NULL THEN c.data ELSE json_set(c.data,
   '$.native_turn_id',json_extract(c.data,'$.turn_id'),'$.turn_id',t.id,
   '$.agent',json_extract(t.data,'$.agent'),'$.provider',json_extract(t.data,'$.provider'),
   '$.model',json_extract(t.data,'$.model'),'$.options',json_extract(t.data,'$.options'),
   '$.mixed_options',json(t.data -> '$.mixed_options'),'$.options_complete',json('true')) END,
  '$.overlap',CASE WHEN json_extract(c.data,'$.adapter')='acp' AND n.native_id IS NOT NULL THEN 'possible_mirror' ELSE 'none_detected' END) AS data
 FROM base_calls c LEFT JOIN durable t ON t.native_session=json_extract(c.data,'$.native_session_id') AND t.rpc=c.rpc_request_id
 LEFT JOIN native_sessions n ON n.native_id=json_extract(c.data,'$.native_session_id');
CREATE TABLE call_facts(id TEXT PRIMARY KEY,source_id TEXT NOT NULL,data TEXT NOT NULL);
PRAGMA user_version=3;
PRAGMA application_id=1280136533;
