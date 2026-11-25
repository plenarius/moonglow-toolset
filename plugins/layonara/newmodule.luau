-- New Layonara Module…: the builder's module folder (nwn-module's nasher target bare), from the builder service.
local api = require("api")

local newmodule = {}

-- Whether the folder has a file in it (hidden ones, as .DS_Store, aside).
local function has_files(folder): boolean
  for _, f in folder:files() do
    if string.sub(f, 1, 1) ~= "." and not string.find(f, "/.", 1, true) then
      return true
    end
  end
  return false
end

function newmodule.run(ctx)
  api.signed_in(ctx)
  local r, data = api.call(ctx, "GET", "/module-folder")
  if not r.ok then
    api.fail(ctx, r, data, "Layonara's module folder")
  end
  if string.sub(r.body, 1, 2) ~= "PK" then
    api.stop("Layonara's builder service sent something that is no zip: tell the staff, with the log.")
  end
  local named = r.headers["content-disposition"] and string.match(r.headers["content-disposition"], 'filename="?([^";]+)')
  local name = if named then string.gsub(named, "%.zip$", "") else "Layonara's module"
  local folder = ctx.ui:open_folder({ title = "Choose an empty folder for Layonara's module" })
  if not folder then
    return
  end
  if has_files(folder) then
    local ok = ctx.ui:confirm(
      `The folder {folder.name} has files in it already. Put Layonara's module there anyway? Files of the same names are replaced.`
    )
    if not ok then
      api.stop(`Nothing was written into {folder.name}: choose an empty folder.`)
    end
  end
  local got = folder:unzip(r.body, { strip = 1 })
  if got.files == 0 then
    api.stop("Layonara's module folder came empty: tell the staff, with the log.")
  end
  folder:open({ target = "bare" })
  ctx.log:info(`{name}: {got.files} files into {folder.name}, opened as the module (nasher target bare)`)
  ctx.log:info("Fetch the areas you work on with Plugins › Layonara › Fetch Area…")
end

return newmodule
