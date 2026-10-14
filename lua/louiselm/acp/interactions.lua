---@diagnostic disable-next-line: undefined-global -- Neovim supplies JSON null/object and array facilities.
local nvim = vim
local M = {}

local METHODS = {
  ["x.ai/ask_user_question"] = "question",
  ["x.ai/exit_plan_mode"] = "plan_review",
}

---@param value unknown
---@return boolean
local function text(value)
  return type(value) == "string" and value ~= ""
end

---@param value unknown
---@param expected string
---@return boolean
local function optional(value, expected)
  return value == nil or value == nvim.NIL or type(value) == expected
end

---@param value unknown
---@return boolean
local function array(value)
  return type(value) == "table" and nvim.islist(value)
end

---Validate the pinned xAI interaction contract and copy only consumed fields.
---Wrapped methods must agree with their inner method; unknown optional fields are ignored.
---@param request louiselm.acp.JsonRpcRequest Direct or ACP-wrapped request.
---@param session_id string? Current Agent-side Session identity.
---@return louiselm.session.InteractionData? data Owned normalized request.
---@return louiselm.acp.JsonRpcError? error Invalid params (-32602) or unsupported method (-32601).
function M.parse(request, session_id)
  local method = request.method
  local wrapped = method:sub(1, 1) == "_"
  if wrapped then
    method = method:sub(2)
  end
  local kind = METHODS[method]
  if kind == nil then
    return nil, { code = -32601, message = "Method not found" }
  end
  local params = request.params
  if wrapped then
    if type(params) ~= "table" or params.method ~= method then
      return nil, { code = -32602, message = "malformed xAI interaction envelope" }
    end
    params = params.params
  end
  if
    type(params) ~= "table"
    or not text(params.sessionId)
    or params.sessionId ~= session_id
    or not text(params.toolCallId)
  then
    return nil, { code = -32602, message = "malformed xAI interaction identity" }
  end
  local data = { kind = kind, request_id = request.id, session_id = params.sessionId, tool_call_id = params.toolCallId }
  if kind == "plan_review" then
    if not optional(params.planContent, "string") then
      return nil, { code = -32602, message = "planContent must be a string or null" }
    end
    data.plan_content = type(params.planContent) == "string" and params.planContent or nil
    return data
  end
  if (params.mode ~= "default" and params.mode ~= "plan") or not array(params.questions) or #params.questions == 0 then
    return nil, { code = -32602, message = "question request requires mode and a non-empty questions array" }
  end
  data.mode, data.questions = params.mode, {}
  local seen = {}
  for _, question in ipairs(params.questions) do
    if
      type(question) ~= "table"
      or not text(question.question)
      or seen[question.question]
      or not array(question.options)
      or not optional(question.multiSelect, "boolean")
    then
      return nil, { code = -32602, message = "malformed or duplicate xAI question" }
    end
    seen[question.question] = true
    local options, labels = {}, {}
    for _, option in ipairs(question.options) do
      if
        type(option) ~= "table"
        or not text(option.label)
        or labels[option.label]
        or type(option.description) ~= "string"
        or not optional(option.preview, "string")
      then
        return nil, { code = -32602, message = "malformed or duplicate xAI question option" }
      end
      labels[option.label] = true
      options[#options + 1] = {
        label = option.label,
        description = option.description,
        preview = type(option.preview) == "string" and option.preview or nil,
      }
    end
    data.questions[#data.questions + 1] =
      { question = question.question, options = options, multi_select = question.multiSelect == true }
  end
  return data
end

---Validate a consumer answer and construct the exact wire result (no extension envelope).
---Accepted answers use arrays; partial_answers use strings and are restricted to plan mode.
---@param data louiselm.session.InteractionData Original owned request.
---@param result unknown Consumer response; malformed/contradictory fields leave the request pending.
---@return louiselm.session.InteractionResponse? response
---@return string? error_message Safe shape diagnostic.
function M.response(data, result)
  if type(result) ~= "table" then
    return nil, "interaction response must be a table"
  end
  local outcome = result.outcome
  if data.kind == "plan_review" then
    if outcome ~= "approved" and outcome ~= "cancelled" and outcome ~= "abandoned" then
      return nil, "invalid plan review outcome"
    end
    if
      not optional(result.feedback, "string")
      or (result.feedback ~= nil and result.feedback ~= nvim.NIL and outcome ~= "cancelled")
      or result.answers ~= nil
      or result.annotations ~= nil
      or result.partial_answers ~= nil
    then
      return nil, "contradictory plan review response"
    end
    return { outcome = outcome, feedback = type(result.feedback) == "string" and result.feedback or nil }
  end
  if result.feedback ~= nil then
    return nil, "question response cannot contain plan feedback"
  end
  if outcome == "cancelled" then
    if result.answers ~= nil or result.annotations ~= nil or result.partial_answers ~= nil then
      return nil, "cancelled question response cannot contain answers"
    end
    return { outcome = "cancelled" }
  end
  local questions = {}
  for _, question in ipairs(data.questions or {}) do
    questions[question.question] = question
  end
  if outcome == "chat_about_this" or outcome == "skip_interview" then
    if data.mode ~= "plan" or result.answers ~= nil or result.annotations ~= nil then
      return nil, "partial answers require a plan-mode outcome"
    end
    local partial = result.partial_answers
    if partial == nil then
      partial = nvim.empty_dict()
    end
    if type(partial) ~= "table" then
      return nil, "partial_answers must be an object of strings"
    end
    local response = nvim.empty_dict()
    for key, value in pairs(partial) do
      if questions[key] == nil or not text(value) then
        return nil, "invalid partial answer"
      end
      response[key] = value
    end
    return { outcome = outcome, partial_answers = response }
  end
  if outcome ~= "accepted" or type(result.answers) ~= "table" or result.partial_answers ~= nil then
    return nil, "accepted question response requires an answers object"
  end
  local answers = nvim.empty_dict()
  for key, values in pairs(result.answers) do
    local question = questions[key]
    if question == nil or not array(values) or #values == 0 or (not question.multi_select and #values > 1) then
      return nil, "invalid question answer selection"
    end
    local allowed, selected = {}, {}
    for _, option in ipairs(question.options) do
      allowed[option.label] = true
    end
    for _, label in ipairs(values) do
      if not text(label) or selected[label] or (not allowed[label] and (label ~= "Other" or #values ~= 1)) then
        return nil, "invalid or duplicate answer label"
      end
      selected[label] = true
    end
    answers[key] = nvim.deepcopy(values)
  end
  if not optional(result.annotations, "table") then
    return nil, "annotations must be an object"
  end
  local annotations = nvim.empty_dict()
  if type(result.annotations) == "table" then
    for key, annotation in pairs(result.annotations) do
      local question, selection = questions[key], answers[key]
      if
        question == nil
        or selection == nil
        or type(annotation) ~= "table"
        or not optional(annotation.preview, "string")
        or not optional(annotation.notes, "string")
      then
        return nil, "invalid question annotation"
      end
      local preview = type(annotation.preview) == "string" and annotation.preview or nil
      if preview ~= nil then
        local expected
        if not question.multi_select and #selection == 1 then
          for _, option in ipairs(question.options) do
            if option.label == selection[1] then
              expected = option.preview
            end
          end
        end
        if expected == nil or preview ~= expected then
          return nil, "preview must match the selected single option verbatim"
        end
      end
      local notes = type(annotation.notes) == "string" and annotation.notes or nil
      if preview ~= nil or notes ~= nil then
        annotations[key] = { preview = preview, notes = notes }
      end
    end
  end
  for key, selection in pairs(answers) do
    if #selection == 1 and selection[1] == "Other" then
      local offered = false
      for _, option in ipairs(questions[key].options) do
        if option.label == "Other" then
          offered = true
        end
      end
      if not offered and (annotations[key] == nil or not text(annotations[key].notes)) then
        return nil, "freeform Other answer requires notes"
      end
    end
  end
  return { outcome = "accepted", answers = answers, annotations = next(annotations) ~= nil and annotations or nil }
end

return M
