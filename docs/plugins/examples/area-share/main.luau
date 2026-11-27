-- Area share: ctx.signin, ctx.net and the lossless JSON of Layonara API 1.
local mg = require("@moonglow")

local SERVICE = "https://areas.example.org/v1/areas/"

mg.command("sign-in", function(ctx)
  local s = ctx.signin:start("example")
  if s.signed_in then
    ctx.log:info("Signed in as " .. (s.name or "someone"))
  else
    ctx.log:warn("Not signed in")
  end
end)

mg.command("send", function(ctx)
  for _, area in ipairs(ctx.selection) do
    local name = area.area
    -- (As raw JSON: a 64-bit field keeps all its digits.)
    local body = {
      are = mg.json.raw(ctx.module:json(name .. ".are")),
      git = mg.json.raw(ctx.module:json(name .. ".git")),
    }
    local r = ctx.net:request({
      method = "POST", url = SERVICE .. name, json = body, signin = "example",
    })
    if not r.ok then
      error(name .. ": the service answered " .. r.status)
    end
    ctx.log:info(name .. ": sent, revision " .. tostring(mg.json.decode(r.body).rev))
  end
end)

mg.command("fetch", function(ctx)
  for _, area in ipairs(ctx.selection) do
    local name = area.area
    local r = ctx.net:request({ url = SERVICE .. name, signin = "example" })
    if not r.ok then
      error(name .. ": the service answered " .. r.status)
    end
    local got = mg.json.decode(r.body)
    ctx.edit:write_json(name .. ".are", got.are)
    ctx.edit:write_json(name .. ".git", got.git)
  end
  return { label = "Fetch Area" }
end)

mg.command("sign-out", function(ctx)
  ctx.signin:stop("example")
  ctx.log:info("Signed out")
end)
