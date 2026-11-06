---@diagnostic disable-next-line: undefined-global -- Neovim owns presentation and scheduling here.
local nvim = vim
local M = {}

---@class louiselm.ui.InteractionChoice
---@field label string
---@field action? string
---@field option? louiselm.session.QuestionOption

---Present one validated questionnaire or plan review in its owned decision prompt.
---Hiding the prompt preserves the current question, selections and input draft.
---@param data louiselm.session.InteractionData Validated request snapshot.
---@param respond fun(result: louiselm.session.InteractionResponse): boolean, string? Exactly-once responder; its owner reports transport failures.
---@param report_error fun(message: string) Payload-free presentation failure.
---@param session_label string Single-line requesting Session identity for all decision UI.
---@param prompt louiselm.ui.DecisionPrompt Prompt owned by this request.
---@return fun() cancel Idempotent UI retirement; the caller owns the ACP cancellation.
function M.open(data, respond, report_error, session_label, prompt)
  local finished = false
  local revision, index = 0, 1
  local selections, notes = {}, {}
  local questions = data.questions or {}

  local function finish(result)
    if finished then
      return
    end
    finished = true
    revision = revision + 1
    prompt:dispose()
    local sent, err = respond(result)
    if not sent then
      return false, err
    end
    return true
  end

  local function run(callback)
    if finished then
      return
    end
    local ok, err = pcall(callback)
    if not ok then
      -- A selector exception can include displayed content; never echo its payload.
      report_error(type(err) == "string" and "interaction UI failed" or "interaction UI raised an error")
      finish({ outcome = "cancelled" })
    end
  end

  local function presented(opened)
    if not opened and not finished then
      report_error("interaction UI failed")
      finish({ outcome = "cancelled" })
    end
  end

  local function provider_callback(callback)
    revision = revision + 1
    local token = revision
    return function(value)
      local function deliver()
        if finished or token ~= revision then
          return
        end
        run(function()
          callback(value)
        end)
      end
      if nvim.in_fast_event() then
        nvim.schedule(deliver)
      else
        deliver()
      end
    end
  end

  local function answers(outcome)
    local result = nvim.empty_dict()
    local annotations = nvim.empty_dict()
    for _, question in ipairs(questions) do
      local chosen = selections[question.question] or {}
      local labels, preview = {}, nil
      for _, option in ipairs(question.options) do
        if chosen[option.label] then
          labels[#labels + 1] = option.label
          if not question.multi_select then
            preview = option.preview
          end
        end
      end
      local note = notes[question.question]
      if #labels == 0 and note ~= nil and note ~= "" then
        labels = { "Other" }
      end
      if #labels > 0 then
        result[question.question] = outcome == "accepted" and labels or table.concat(labels, ", ")
        if outcome == "accepted" and (preview ~= nil or note ~= nil) then
          annotations[question.question] = { preview = preview, notes = note }
        end
      end
    end
    if outcome ~= "accepted" then
      return { outcome = outcome, partial_answers = result }
    end
    return { outcome = outcome, answers = result, annotations = next(annotations) ~= nil and annotations or nil }
  end

  local present
  local function inspect(content, title)
    presented(
      prompt:inspect(
        nvim.split(content, "\n", { plain = true }),
        session_label .. " · " .. title .. " · q/Esc: back to choices",
        provider_callback(function()
          present()
        end)
      )
    )
  end

  present = function()
    ---@type louiselm.ui.InteractionChoice[]
    local choices = {}
    local function action(label, name)
      choices[#choices + 1] = { label = label, action = name }
    end
    local question = questions[index]
    if data.kind == "plan_review" then
      action("Request changes", "feedback")
      action("Abandon", "abandon")
      if data.plan_content ~= nil then
        action("View plan", "preview")
      end
      action("Approve", "approve")
    else
      local selected = selections[question.question] or {}
      selections[question.question] = selected
      for _, option in ipairs(question.options) do
        choices[#choices + 1] = {
          option = option,
          label = (selected[option.label] and "[x] " or "[ ] ") .. option.label .. " — " .. option.description,
        }
      end
      action("Other / add notes", "notes")
      if not question.multi_select then
        for _, option in ipairs(question.options) do
          if selected[option.label] and option.preview ~= nil then
            action("View selected preview", "preview")
          end
        end
      end
      action(index == #questions and "Submit answers" or "Next question", "next")
      if data.mode == "plan" then
        action("Chat about this", "chat_about_this")
        action("Skip interview", "skip_interview")
      end
      action("Cancel", "cancel")
    end
    presented(prompt:select(
      choices,
      {
        prompt = session_label
          .. " · "
          .. (
            data.kind == "plan_review" and "louiselm plan review: "
            or (
              "louiselm question "
              .. index
              .. "/"
              .. #questions
              .. ": "
              .. question.question:gsub("[\r\n]", " ")
              .. " "
            )
          ),
        format_item = function(choice)
          return (choice.label:gsub("[\r\n]", " "))
        end,
      },
      provider_callback(function(choice)
        if choice == nil then
          finish(data.kind == "plan_review" and { outcome = "abandoned" } or { outcome = "cancelled" })
        elseif choice.option ~= nil then
          local selected = selections[question.question]
          local was_selected = selected[choice.option.label]
          if not question.multi_select then
            selected = {}
            selections[question.question] = selected
          end
          selected[choice.option.label] = not was_selected
          present()
        elseif choice.action == "approve" then
          finish({ outcome = "approved" })
        elseif choice.action == "abandon" then
          finish({ outcome = "abandoned" })
        elseif choice.action == "cancel" then
          finish({ outcome = "cancelled" })
        elseif choice.action == "feedback" then
          presented(
            prompt:input(
              { prompt = session_label .. " · Plan feedback: " },
              provider_callback(function(feedback)
                finish({ outcome = "cancelled", feedback = feedback ~= "" and feedback or nil })
              end)
            )
          )
        elseif choice.action == "notes" then
          presented(
            prompt:input(
              { prompt = session_label .. " · Answer / notes: ", default = notes[question.question] },
              provider_callback(function(note)
                if note ~= nil then
                  notes[question.question] = note ~= "" and note or nil
                end
                present()
              end)
            )
          )
        elseif choice.action == "next" then
          if index == #questions then
            finish(answers("accepted"))
          else
            index = index + 1
            present()
          end
        elseif choice.action == "chat_about_this" or choice.action == "skip_interview" then
          finish(answers(choice.action))
        elseif choice.action == "preview" then
          local content = data.plan_content
          if data.kind == "question" then
            for _, option in ipairs(question.options) do
              if selections[question.question][option.label] then
                content = option.preview
              end
            end
          end
          inspect(content or "", data.kind == "question" and "Option preview" or "Proposed plan")
        end
      end)
    ))
  end
  run(present)
  return function()
    if finished then
      return
    end
    finished = true
    revision = revision + 1
    prompt:dispose()
  end
end

return M
