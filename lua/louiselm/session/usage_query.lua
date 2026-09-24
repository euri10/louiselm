---@diagnostic disable-next-line: undefined-global -- JSON and input validation use Neovim facilities.
local nvim = vim
local M = {}

-- Shared by the options picker and explorer. Callers populate cohort_turns
-- (candidate, turns.*, outcome_data); the reader owns history_guard.
M.metrics = [[
INSERT INTO history_guard SELECT NOT EXISTS (
 SELECT 1 FROM cohort_turns t WHERE t.agent='' OR t.provider='' OR t.acp_session_id=''
 OR julianday(t.prepared_at) IS NULL OR json_type(t.options) IS NOT 'object'
 OR EXISTS(SELECT 1 FROM json_each(t.options) o WHERE o.key='' OR o.type NOT IN ('text','true','false'))
 OR (t.model IS NOT NULL AND json_type(t.model) NOT IN ('text','true','false'))
);
-- Refuse malformed consumed telemetry rather than letting SQLite coerce it.
INSERT INTO history_guard SELECT NOT EXISTS (
 SELECT 1 FROM cohort_turns t WHERE t.outcome_data IS NOT NULL AND (
  json_type(t.outcome_data) IS NOT 'object'
  OR json_extract(t.outcome_data,'$.outcome') NOT IN ('completed','cancelled','failed','disposed','not_sent')
  OR json_type(t.outcome_data,'$.outcome') IS NOT 'text'
  OR json_type(t.outcome_data,'$.peer_response') NOT IN ('true','false')
  OR json_type(t.outcome_data,'$.peer_response') IS NULL
  OR (json_type(t.outcome_data,'$.usage') IS NOT NULL AND json_type(t.outcome_data,'$.usage') IS NOT 'object')
  OR EXISTS (SELECT 1 FROM json_each(t.outcome_data,'$.usage') u WHERE
   u.key NOT IN ('total_tokens','input_tokens','output_tokens','thought_tokens','cached_read_tokens','cached_write_tokens')
   OR u.type IS NOT 'integer' OR u.value<0)
 ));
CREATE TEMP TABLE cost_readings AS
SELECT candidate,id,0 AS sequence,cost_baseline AS cost FROM cohort_turns
UNION ALL
SELECT t.candidate,t.id,e.sequence,json_extract(e.data,'$.cost')
FROM cohort_turns t JOIN turn_events e ON e.turn_id=t.id AND e.kind='cost';
INSERT INTO history_guard SELECT NOT EXISTS (
 SELECT 1 FROM cost_readings WHERE cost IS NOT NULL AND json_type(cost) IS NOT 'null' AND (
  json_type(cost) IS NOT 'object' OR (SELECT count(*) FROM json_each(cost))<>2
  OR json_type(cost,'$.amount') NOT IN ('integer','real') OR json_type(cost,'$.amount') IS NULL
  OR json_extract(cost,'$.amount')<0 OR json_extract(cost,'$.amount')>=1e999
  OR json_type(cost,'$.currency') IS NOT 'text'
  OR json_extract(cost,'$.currency') NOT GLOB '[A-Z][A-Z][A-Z]'
 ));
CREATE TEMP TABLE metrics AS
WITH ordered_costs AS (
 SELECT *,json_extract(cost,'$.amount') AS amount,json_extract(cost,'$.currency') AS currency,
 lag(json_extract(cost,'$.amount')) OVER (PARTITION BY candidate,id ORDER BY sequence) AS previous,
 lag(json_extract(cost,'$.currency')) OVER (PARTITION BY candidate,id ORDER BY sequence) AS previous_currency
 FROM cost_readings
), deltas AS (
 SELECT candidate,id,min(currency) AS currency,max(amount)-min(amount) AS amount
 FROM ordered_costs GROUP BY candidate,id
 HAVING count(*)>1 AND count(amount)=count(*)
 AND sum(CASE WHEN sequence>0 AND (previous IS NULL OR amount<previous OR currency IS NOT previous_currency) THEN 1 ELSE 0 END)=0
), metrics AS (
 SELECT candidate,'turns' AS kind,'' AS name,count(*) AS samples,NULL AS average,NULL AS total FROM cohort_turns GROUP BY candidate
 UNION ALL
 SELECT candidate,'outcome',coalesce(json_extract(outcome_data,'$.outcome'),'unobserved'),count(*),NULL,NULL
 FROM cohort_turns GROUP BY candidate,coalesce(json_extract(outcome_data,'$.outcome'),'unobserved')
 UNION ALL
 SELECT t.candidate,'token',u.key,count(*),avg(u.value),sum(u.value)
 FROM cohort_turns t,json_each(t.outcome_data,'$.usage') u GROUP BY t.candidate,u.key
 UNION ALL
 SELECT d.candidate,'cost',d.currency,count(*),avg(d.amount),sum(d.amount) FROM deltas d
 JOIN cohort_turns t ON t.candidate=d.candidate AND t.id=d.id
 WHERE json_extract(t.outcome_data,'$.peer_response')=1 GROUP BY d.candidate,d.currency
)
SELECT * FROM metrics;

]]

---@alias louiselm.session.UsageDimension string Agent/provider/model/session or option:<recorded ID>.
---@class louiselm.session.UsageQuery
---@field view? "summary"|"turns"|"events"|"dimensions" Defaults to summary.
---@field filters? table<louiselm.session.UsageDimension, string|boolean|userdata> Conjunction; vim.NIL matches an absent dimension. Turns use starting values.
---@field group_by? louiselm.session.UsageDimension[] Defaults to no dimensions.
---@field bucket? "none"|"hour"|"day" UTC buckets of prepared_at; defaults to none.
---@field from? string Inclusive UTC timestamp YYYY-MM-DDTHH:MM:SS[.fff]Z.
---@field until_time? string Exclusive UTC timestamp.
---@field mixed? "include"|"exclude"|"only" Defaults to include; fixed-configuration summaries still exclude mixed turns.
---@field turn_id? string Exact durable turn; events includes that turn's observations and associated transitions.
---@field limit? integer Page size, 1..100; defaults to 25.
---@field offset? integer Zero-based page offset; defaults to zero.
---@class louiselm.session.UsageTotals: louiselm.session.CohortSummary
---@field tokens table<string, louiselm.session.UsageTotalMetric>
---@field costs louiselm.session.UsageTotalCurrency[]
---@class louiselm.session.UsageTotalMetric: louiselm.session.UsageMetric
---@field total number Sum over measured turns only.
---@class louiselm.session.UsageTotalCurrency: louiselm.session.UsageTotalMetric
---@field currency string
---@class louiselm.session.UsageRow
---@field dimensions? table<string, string|boolean> Summary group; absent values are omitted.
---@field bucket_start? string Inclusive UTC bucket boundary.
---@field bucket_end? string Exclusive UTC bucket boundary.
---@field summary? louiselm.session.UsageTotals
---@field id? string Durable turn ID or option event ID.
---@field agent? string
---@field provider? string Starting resolved Provider.
---@field acp_session_id? string
---@field prepared_at? string
---@field model? string|boolean Starting advertised Model.
---@field options? table<string, string|boolean> Immutable starting tuple.
---@field cost_baseline? louiselm.session.Cost
---@field mixed? boolean Changed during turn; excluded from fixed-configuration comparisons.
---@field kind? string Event kind.
---@field observed_at? string Event timestamp.
---@field sequence? integer Order within the recorded stream.
---@field observer_id? string Option observation stream identity.
---@field turn_id? string Associated durable turn, only when recorded.
---@field data? table Normalized event metadata, including observed source and established request links.
---@field dimension? string Dimension discovery row.
---@field value? string|boolean Recorded dimension value.
---@class louiselm.session.UsagePage
---@field timezone "UTC"
---@field view string
---@field total integer Matching rows, independent of the page size.
---@field next_offset? integer Absent on the last page.
---@field mixed_turns integer Mixed turns matching starting-value filters.
---@field excluded_mixed integer Matching mixed turns excluded from this summary.
---@field summary louiselm.session.UsageTotals Matching dispatched turns; no Lua aggregation.
---@field rows louiselm.session.UsageRow[]
---@alias louiselm.session.UsageQueryCallback fun(page: louiselm.session.UsagePage?, error?: louiselm.session.RecordingError)

local fields = {
  view = true,
  filters = true,
  group_by = true,
  bucket = true,
  from = true,
  until_time = true,
  mixed = true,
  turn_id = true,
  limit = true,
  offset = true,
}
local views = { summary = true, turns = true, events = true, dimensions = true }
local buckets = { none = true, hour = true, day = true }
local mixed_modes = { include = true, exclude = true, only = true }
local dimensions = { agent = "agent", provider = "provider", model = "model", session = "acp_session_id" }

local function nonempty(value)
  return type(value) == "string" and value ~= ""
end

local function dimension(value)
  return type(value) == "string" and (dimensions[value] ~= nil or value:match("^option:.+") ~= nil)
end

local function timestamp(value)
  if type(value) ~= "string" then
    return false
  end
  local year, month, day, hour, minute, second, fraction =
    value:match("^(%d%d%d%d)%-(%d%d)%-(%d%d)T(%d%d):(%d%d):(%d%d)(.-)Z$")
  if not year or (fraction ~= "" and not fraction:match("^%.%d%d?%d?$")) then
    return false
  end
  year, month, day = tonumber(year), tonumber(month), tonumber(day)
  local days = {
    31,
    (year % 4 == 0 and (year % 100 ~= 0 or year % 400 == 0)) and 29 or 28,
    31,
    30,
    31,
    30,
    31,
    31,
    30,
    31,
    30,
    31,
  }
  return month >= 1
    and month <= 12
    and day >= 1
    and day <= days[month]
    and tonumber(hour) < 24
    and tonumber(minute) < 60
    and tonumber(second) < 60
end

---@param query louiselm.session.UsageQuery
---@return boolean
local function valid(query)
  if type(query) ~= "table" then
    return false
  end
  for key in pairs(query) do
    if not fields[key] then
      return false
    end
  end
  if
    (query.view ~= nil and not views[query.view])
    or (query.bucket ~= nil and not buckets[query.bucket])
    or (query.mixed ~= nil and not mixed_modes[query.mixed])
  then
    return false
  end
  for _, field in ipairs({ "limit", "offset" }) do
    local value = query[field]
    if
      value ~= nil
      and (
        type(value) ~= "number"
        or value % 1 ~= 0
        or value < (field == "limit" and 1 or 0)
        or value > (field == "limit" and 100 or 2147483647)
      )
    then
      return false
    end
  end
  if
    (query.from ~= nil and not timestamp(query.from))
    or (query.until_time ~= nil and not timestamp(query.until_time))
    or (query.turn_id ~= nil and not nonempty(query.turn_id))
  then
    return false
  end
  -- Canonical seconds plus a normalized fractional suffix compare chronologically.
  if query.from and query.until_time then
    local function key(value)
      return value:sub(1, 19) .. ((value:match("%.(%d+)Z$") or "") .. "000"):sub(1, 3)
    end
    if key(query.from) >= key(query.until_time) then
      return false
    end
  end
  if query.filters ~= nil then
    if type(query.filters) ~= "table" then
      return false
    end
    for key, value in pairs(query.filters) do
      if
        not dimension(key)
        or (value ~= nvim.NIL and type(value) ~= "string" and type(value) ~= "boolean")
        or (value ~= nvim.NIL and dimensions[key] and key ~= "model" and type(value) ~= "string")
      then
        return false
      end
    end
  end
  if query.group_by ~= nil then
    if type(query.group_by) ~= "table" or not nvim.islist(query.group_by) then
      return false
    end
    local seen = {}
    for _, key in ipairs(query.group_by) do
      if not dimension(key) or seen[key] then
        return false
      end
      seen[key] = true
    end
  end
  return true
end

-- SQL expressions only contain fixed grammar and hex-quoted data.
local function value_sql(key, quote, alias)
  if key == "model" then
    return alias .. ".model"
  end
  if dimensions[key] then
    return "json_quote(" .. alias .. "." .. dimensions[key] .. ")"
  end
  return "(SELECT CASE o.type WHEN 'true' THEN 'true' WHEN 'false' THEN 'false' ELSE json_quote(o.value) END FROM json_each("
    .. alias
    .. ".options) o WHERE o.key="
    .. quote(key:sub(8))
    .. ")"
end

local function predicates(query, quote, alias, with_time)
  local clauses = {}
  for key, value in pairs(query.filters or {}) do
    clauses[#clauses + 1] = value_sql(key, quote, alias)
      .. " IS "
      .. (value == nvim.NIL and "NULL" or quote(nvim.json.encode(value)))
  end
  if query.turn_id then
    clauses[#clauses + 1] = alias .. ".id=" .. quote(query.turn_id)
  end
  if with_time then
    if query.from then
      clauses[#clauses + 1] = "julianday(" .. alias .. ".prepared_at)>=julianday(" .. quote(query.from) .. ")"
    end
    if query.until_time then
      clauses[#clauses + 1] = "julianday(" .. alias .. ".prepared_at)<julianday(" .. quote(query.until_time) .. ")"
    end
  end
  return #clauses > 0 and table.concat(clauses, " AND ") or "1"
end

local function summary_sql(candidate)
  local where = " FROM metrics WHERE candidate=" .. candidate
  return [[json_object('turns',coalesce((SELECT samples]]
    .. where
    .. [[ AND kind='turns'),0),
    'outcomes',(SELECT json_group_object(name,samples)]]
    .. where
    .. [[ AND kind='outcome'),
    'tokens',(SELECT json_group_object(name,json_object('samples',samples,'average',average,'total',total))]]
    .. where
    .. [[ AND kind='token'),
    'costs',(SELECT json_group_array(json_object('currency',name,'samples',samples,'average',average,'total',total))
      FROM (SELECT *]]
    .. where
    .. [[ AND kind='cost' ORDER BY name)))]]
end

---Build the explorer's read-only SQL, or return nil for a closed-schema validation failure.
---The supplied encoder is the recorder's existing safe SQL text boundary.
---@param query louiselm.session.UsageQuery
---@param quote fun(value: string?): string
---@return string? sql
function M.build(query, quote)
  if not valid(query) then
    return nil
  end
  local view, bucket = query.view or "summary", query.bucket or "none"
  local limit, offset = query.limit or 25, query.offset or 0
  local fixed = false
  for key in pairs(query.filters or {}) do
    if key ~= "agent" and key ~= "session" then
      fixed = true
    end
  end
  for _, key in ipairs(query.group_by or {}) do
    if key ~= "agent" and key ~= "session" then
      fixed = true
    end
  end
  local exclude = view == "summary" and fixed
  local sql = {
    [[
CREATE TEMP TABLE matching_turns AS
SELECT t.*,o.data AS outcome_data,
 EXISTS(SELECT 1 FROM option_events e WHERE e.turn_id=t.id) AS mixed
FROM turns t LEFT JOIN turn_events o ON o.turn_id=t.id AND o.kind='outcome'
WHERE EXISTS(SELECT 1 FROM turn_events d WHERE d.turn_id=t.id AND d.kind='dispatch') AND
]] .. predicates(query, quote, "t", view ~= "events") .. ";",
  }
  local mixed = query.mixed or "include"
  sql[#sql + 1] = "CREATE TEMP TABLE selected_turns AS SELECT * FROM matching_turns WHERE "
    .. ((exclude or mixed == "exclude") and "mixed=0" or mixed == "only" and "mixed=1" or "1")
    .. ((exclude and mixed == "only") and " AND 0" or "")
    .. ";"
  local page_rows, total
  if view == "summary" and bucket == "none" and #(query.group_by or {}) == 0 then
    -- The ungrouped row and overall totals describe the same cohort.
    sql[#sql + 1] = "CREATE TEMP TABLE cohort_turns AS SELECT 0 AS candidate,t.* FROM selected_turns t;"
    total = "EXISTS(SELECT 1 FROM selected_turns)"
    page_rows = [[SELECT json_object('dimensions',json('{}'),'bucket_start',NULL,'bucket_end',NULL,'summary',]]
      .. summary_sql("0")
      .. ") AS data WHERE "
      .. total
      .. " LIMIT "
      .. limit
      .. " OFFSET "
      .. offset
  elseif view == "summary" then
    local group_fields = {}
    for _, key in ipairs(query.group_by or {}) do
      group_fields[#group_fields + 1] = quote(key)
      group_fields[#group_fields + 1] = "json(" .. value_sql(key, quote, "t") .. ")"
    end
    local start, finish = "NULL", "NULL"
    if bucket ~= "none" then
      local format = bucket == "hour" and "%Y-%m-%dT%H:00:00Z" or "%Y-%m-%dT00:00:00Z"
      start = "strftime(" .. quote(format) .. ",t.prepared_at)"
      finish = "strftime("
        .. quote(format)
        .. ",t.prepared_at,"
        .. quote(bucket == "hour" and "+1 hour" or "+1 day")
        .. ")"
    end
    sql[#sql + 1] = "CREATE TEMP TABLE grouped_turns AS SELECT t.*,json_object("
      .. table.concat(group_fields, ",")
      .. ") AS dimensions,"
      .. start
      .. " AS bucket_start,"
      .. finish
      .. " AS bucket_end FROM selected_turns t;"
    sql[#sql + 1] = [[CREATE TEMP TABLE groups AS SELECT row_number() OVER(ORDER BY bucket_start,dimensions) AS candidate,
dimensions,bucket_start,bucket_end FROM grouped_turns GROUP BY bucket_start,dimensions;
CREATE TEMP TABLE page_groups AS SELECT * FROM groups ORDER BY candidate LIMIT ]] .. limit .. " OFFSET " .. offset .. ";"
    sql[#sql + 1] = [[CREATE TEMP TABLE cohort_turns AS SELECT 0 AS candidate,t.* FROM selected_turns t
UNION ALL SELECT g.candidate,t.id,t.agent,t.provider,t.acp_session_id,t.prepared_at,t.options,t.model,t.cost_baseline,t.outcome_data,t.mixed
FROM grouped_turns t JOIN page_groups g ON t.dimensions=g.dimensions AND t.bucket_start IS g.bucket_start;]]
    total = "(SELECT count(*) FROM groups)"
    page_rows = "SELECT json_object('dimensions',json(g.dimensions),'bucket_start',g.bucket_start,'bucket_end',g.bucket_end,'summary',"
      .. summary_sql("g.candidate")
      .. ") AS data FROM page_groups g ORDER BY candidate"
  elseif view == "turns" then
    sql[#sql + 1] = "CREATE TEMP TABLE page_turns AS SELECT row_number() OVER(ORDER BY julianday(prepared_at),id) AS candidate,* FROM selected_turns ORDER BY julianday(prepared_at),id LIMIT "
      .. limit
      .. " OFFSET "
      .. offset
      .. ";"
    sql[#sql + 1] = [[CREATE TEMP TABLE cohort_turns AS SELECT 0 AS candidate,t.* FROM selected_turns t
UNION ALL SELECT * FROM page_turns;]]
    total = "(SELECT count(*) FROM selected_turns)"
    page_rows = [[SELECT json_object('id',t.id,'agent',t.agent,'provider',t.provider,'acp_session_id',t.acp_session_id,
'prepared_at',t.prepared_at,'model',json(t.model),'options',json(t.options),'cost_baseline',json(t.cost_baseline),
'mixed',json(CASE mixed WHEN 1 THEN 'true' ELSE 'false' END),'summary',]] .. summary_sql("t.candidate") .. ") AS data FROM page_turns t ORDER BY candidate"
  else
    sql[#sql + 1] = "CREATE TEMP TABLE cohort_turns AS SELECT 0 AS candidate,t.* FROM selected_turns t;"
    if view == "dimensions" then
      sql[#sql + 1] = [[CREATE TEMP TABLE dimensions AS
SELECT 'agent' AS dimension,json_quote(agent) AS value FROM selected_turns
UNION SELECT 'provider',json_quote(provider) FROM selected_turns
UNION SELECT 'model',model FROM selected_turns WHERE model IS NOT NULL
UNION SELECT 'session',json_quote(acp_session_id) FROM selected_turns
UNION SELECT 'option:'||o.key,CASE o.type WHEN 'true' THEN 'true' WHEN 'false' THEN 'false' ELSE json_quote(o.value) END
FROM selected_turns t,json_each(t.options) o;]]
      total = "(SELECT count(*) FROM dimensions)"
      page_rows = "SELECT json_object('dimension',dimension,'value',json(value)) AS data FROM dimensions ORDER BY dimension,value LIMIT "
        .. limit
        .. " OFFSET "
        .. offset
    else
      local between = query.turn_id == nil and mixed ~= "only" and not fixed
      local event_clauses = { "e.turn_id IS NULL" }
      for _, key in ipairs({ "agent", "session" }) do
        local value = (query.filters or {})[key]
        if value ~= nil then
          event_clauses[#event_clauses + 1] = "e."
            .. dimensions[key]
            .. "="
            .. quote(type(value) == "string" and value or nil)
        end
      end
      sql[#sql + 1] = [[CREATE TEMP TABLE events AS
SELECT e.turn_id||':'||e.sequence AS id,e.kind,e.turn_id,e.observed_at,e.sequence,
e.turn_id AS stream,NULL AS observer_id,e.data
FROM turn_events e JOIN selected_turns t ON t.id=e.turn_id
UNION ALL SELECT e.id,'options',e.turn_id,e.observed_at,e.sequence,e.observer_id,e.observer_id,
json_object('agent',e.agent,'acp_session_id',e.acp_session_id,'previous_options',json(e.previous_options),
'options',json(e.options),'source',e.source,'request',json(e.request))
FROM option_events e WHERE e.turn_id IN (SELECT id FROM selected_turns)]] .. (between and " OR (" .. table.concat(
        event_clauses,
        " AND "
      ) .. ")" or "") .. ";"
      local time = {}
      if query.from then
        time[#time + 1] = "julianday(observed_at)>=julianday(" .. quote(query.from) .. ")"
      end
      if query.until_time then
        time[#time + 1] = "julianday(observed_at)<julianday(" .. quote(query.until_time) .. ")"
      end
      sql[#sql + 1] = "CREATE TEMP TABLE selected_events AS SELECT * FROM events WHERE "
        .. (#time > 0 and table.concat(time, " AND ") or "1")
        .. ";"
      total = "(SELECT count(*) FROM selected_events)"
      page_rows = [[SELECT json_object('id',id,'kind',kind,'turn_id',turn_id,'observed_at',observed_at,
'sequence',sequence,'observer_id',observer_id,'data',json(data)) AS data FROM selected_events
ORDER BY julianday(observed_at),stream,sequence,id LIMIT ]] .. limit .. " OFFSET " .. offset
    end
  end
  sql[#sql + 1] = M.metrics
  sql[#sql + 1] = "SELECT json_object('timezone','UTC','view',"
    .. quote(view)
    .. ",'total',"
    .. total
    .. ",'next_offset',CASE WHEN "
    .. total
    .. ">"
    .. (offset + limit)
    .. " THEN "
    .. (offset + limit)
    .. " END,"
    .. "'mixed_turns',(SELECT count(*) FROM matching_turns WHERE mixed=1),"
    .. "'excluded_mixed',"
    .. ((exclude or mixed == "exclude") and "(SELECT count(*) FROM matching_turns WHERE mixed=1)" or "0")
    .. ","
    .. "'summary',"
    .. summary_sql("0")
    .. ",'rows',(SELECT json_group_array(json(data)) FROM ("
    .. page_rows
    .. "))) AS result;"
  return table.concat(sql, "\n")
end

return M
