-- term41 Lua status script example.
--
-- Install as:
--   ~/.config/term41/scripts/sys_info.lua
--
-- Suggested config:
--   status_line = "indicator"
--
--   [security.scripts.sys_info]
--   filesystem = true
--
-- This example reads Linux /proc files directly. It does not need shell,
-- process_info, or resource_usage permissions.

local terminal = require("terminal")

local previous_cpu_idle = nil
local previous_cpu_total = nil

local function read_first_line(path)
  local file = io.open(path, "r")
  if file == nil then
    return nil
  end

  local line = file:read("*l")
  file:close()
  return line
end

local function parse_cpu_line(line)
  if line == nil then
    return nil, nil
  end

  local values = {}
  for raw in line:gmatch("%d+") do
    values[#values + 1] = tonumber(raw)
  end

  local idle = (values[4] or 0) + (values[5] or 0)
  local total = 0
  for _, value in ipairs(values) do
    total = total + value
  end

  return idle, total
end

local function sample_cpu_usage()
  local idle, total = parse_cpu_line(read_first_line("/proc/stat"))
  if idle == nil or total == nil then
    return nil
  end

  local previous_idle = previous_cpu_idle
  local previous_total = previous_cpu_total
  previous_cpu_idle = idle
  previous_cpu_total = total

  if previous_idle == nil or previous_total == nil then
    return nil
  end

  local idle_delta = idle - previous_idle
  local total_delta = total - previous_total
  if total_delta <= 0 then
    return nil
  end

  return math.max(0, math.min(100, (1 - idle_delta / total_delta) * 100))
end

local function sample_memory_usage()
  local file = io.open("/proc/meminfo", "r")
  if file == nil then
    return nil
  end

  local total_kib = nil
  local available_kib = nil

  for line in file:lines() do
    local key, value = line:match("^(%w+):%s+(%d+)")
    if key == "MemTotal" then
      total_kib = tonumber(value)
    elseif key == "MemAvailable" then
      available_kib = tonumber(value)
    end

    if total_kib ~= nil and available_kib ~= nil then
      break
    end
  end

  file:close()

  if total_kib == nil or available_kib == nil or total_kib <= 0 then
    return nil
  end

  local used_kib = total_kib - available_kib
  return {
    used_gib = used_kib / 1024 / 1024,
    total_gib = total_kib / 1024 / 1024,
    percent = used_kib / total_kib * 100,
  }
end

local function render_status()
  local cpu = sample_cpu_usage()
  local memory = sample_memory_usage()

  local cpu_text = "CPU --%"
  if cpu ~= nil then
    cpu_text = string.format("CPU %.0f%%", cpu)
  end

  local memory_text = "MEM unavailable"
  if memory ~= nil then
    memory_text = string.format(
      "MEM %.1f/%.1f GiB %.0f%%",
      memory.used_gib,
      memory.total_gib,
      memory.percent
    )
  end

  terminal.set_status_text(" ⟫ " .. cpu_text .. "  " .. memory_text)
end

function sleep(seconds)
  local timer = io.popen("sleep " .. seconds)
  timer:close()
end

render_status()

function update()
  render_status()
  sleep(5)
end
