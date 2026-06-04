// ── Setup ─────────────────────────────────────────────────────
var PROJECT = window.KANBAN_PROJECT || 'unknown';
var BASE = '/api/v1/team/projects/' + PROJECT;

// ── Data loading ──────────────────────────────────────────────

var _cachedStages = [];
var _cachedWorks = [];

// ── XHR-based fetch polyfill (WKWebView sandbox blocks native fetch) ──
function _xhrFetch(url, opts) {
  return new Promise(function(resolve, reject) {
    var xhr = new XMLHttpRequest();
    xhr.open((opts && opts.method) || 'GET', url);
    if (opts && opts.headers) {
      Object.keys(opts.headers).forEach(function(k) { xhr.setRequestHeader(k, opts.headers[k]); });
    }
    xhr.onload = function() {
      resolve({ ok: xhr.status >= 200 && xhr.status < 300, status: xhr.status, json: function() { return Promise.resolve(JSON.parse(xhr.responseText)); }, text: function() { return Promise.resolve(xhr.responseText); } });
    };
    xhr.onerror = function() { reject(new Error('XHR error')); };
    xhr.ontimeout = function() { reject(new Error('XHR timeout')); };
    xhr.send(opts && opts.body);
  });
}

async function load() {
  try {
    var url = BASE + "/works";
    var r = await _xhrFetch(url);
    if (!r.ok) { console.error('fetch failed', r.status); return; }
    var data = await r.json();
    _cachedStages = data.stages || [];
    _cachedWorks = data.works || [];
    render(_cachedStages, _cachedWorks);
  } catch(e) {
    console.error(e);
  }
}

function render(stages, works) {
  var byStage = {};
  for (var i = 0; i < works.length; i++) {
    var w = works[i];
    var s = w.current_stage || "";
    byStage[s] = byStage[s] || [];
    byStage[s].push(w);
  }
  for (var j = 0; j < stages.length; j++) {
    var s = stages[j];
    var list = document.getElementById("list-" + s.id);
    var count = document.getElementById("count-" + s.id);
    if (!list) continue;
    var items = byStage[s.id] || [];
    if (count) count.textContent = items.length;
    if (items.length === 0) {
      list.innerHTML = '<div class="empty-state">No work items</div>';
    } else {
      var html = "";
      for (var k = 0; k < items.length; k++) {
        var w = items[k];
        html += '<div class="card" ' +
                'onclick="openWorkDetail(\'' + esc(w.id) + '\')">' +
                '<button class="card-delete-btn" onclick="deleteWork(\'' + esc(w.id) + '\', event)" title="Delete">×</button>' +
                '<div class="card-title">' + esc(w.title) + '</div>' +
                '<div class="card-meta">' + esc(w.priority || 'normal') + (w.assignee ? ' &middot; ' + esc(w.assignee) : '') + '</div>' +
                (w.output_type ? '<span class="card-output-badge">' + esc(w.output_type) + '</span>' : '') +
                '</div>';
      }
      list.innerHTML = html;
    }
  }
}

// ── Works ─────────────────────────────────────────────────────

function openCreate(stageId) {
  document.getElementById("newStage").value = stageId;
  document.getElementById("newTitle").value = "";
  document.getElementById("newDesc").value = "";
  document.getElementById("newOutputType").value = "";
  document.getElementById("newOutputDesc").value = "";
  document.getElementById("createModal").classList.add("open");
  setTimeout(function(){ document.getElementById("newTitle").focus(); }, 100);
}

function closeCreate() {
  document.getElementById("createModal").classList.remove("open");
}

async function createWork() {
  var title = document.getElementById("newTitle").value.trim();
  if (!title) return;
  var stageId = document.getElementById("newStage").value;
  try {
    await _xhrFetch(BASE + "/works/create", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({
        title: title,
        description: document.getElementById("newDesc").value,
        output_type: document.getElementById("newOutputType").value,
        output_description: document.getElementById("newOutputDesc").value
      })
    });
    closeCreate();
    load();
  } catch(e) { console.error(e); }
}

// ── Delete work item ─────────────────────────────────────────────
//
// When running inside the Tauri desktop app (in an iframe), we request a
// native OS confirmation dialog via postMessage to the parent App.svelte.
// We wait indefinitely for the response — no timeout, no fallback race.
//
// When running standalone (not in an iframe, e.g. direct browser access),
// we use the browser's built-in confirm() dialog.

function requestConfirm(title, message, confirmLabel, cancelLabel) {
  // Standalone browser — use built-in confirm directly
  if (window.parent === window) {
    return Promise.resolve(confirm(message));
  }

  // Inside Tauri iframe — request native OS dialog via parent bridge
  return new Promise(function(resolve) {
    window.parent.postMessage({
      type: "aman:confirm",
      title: title,
      message: message,
      confirmLabel: confirmLabel,
      cancelLabel: cancelLabel
    }, "*");

    function handler(event) {
      if (event.data && event.data.type === "aman:confirm-result") {
        window.removeEventListener("message", handler);
        resolve(event.data.confirmed);
      }
    }
    window.addEventListener("message", handler);
  });
}

function deleteWork(workId, event) {
  if (event) event.stopPropagation(); // prevent card click → open detail

  // Fetch work title for the confirmation message
  var work = (_cachedWorks || []).find(function(w) { return w.id === workId; });
  var titleLine = work ? "「" + work.title + "」" : workId;

  requestConfirm(
    "aman — Delete Work Item",
    "Delete work item " + titleLine + "?\n\nThis action cannot be undone. All context history and output files will be permanently removed.",
    "Delete",
    "Cancel"
  ).then(function(confirmed) {
    if (confirmed) executeDelete(workId);
  });
}

async function executeDelete(workId) {
  try {
    var r = await _xhrFetch(BASE + "/works/" + encodeURIComponent(workId), {
      method: "DELETE"
    });
    if (!r.ok) {
      var e = await r.json().catch(function(){ return {}; });
      flash("Delete failed: " + (e.error || r.statusText), "error");
      return;
    }
    flash("Work item deleted", "success");
    // Close detail modal if the deleted work is currently open
    if (_detailWorkId === workId) {
      closeWorkDetail();
      _detailWorkId = "";
      _detailWork = null;
    }
    load();
  } catch(e) {
    flash("Network error: " + e.message, "error");
    console.error("executeDelete error:", e);
  }
}

async function moveWork(workId) {
  var stages = _cachedStages;
  var work = (_cachedWorks || []).find(function(w) { return w.id === workId; });
  if (!work) { flash("Work item not found: " + workId, "error"); return; }
  var idx = stages.findIndex(function(s) { return s.id === work.current_stage; });
  console.log("moveWork", {workId: workId, currentStage: work.current_stage, stageIdx: idx, stages: stages});
  var nextIds = idx >= 0 ? (stages[idx].allowed_next || []) : [];
  if (nextIds.length === 0) { flash("No next stage available from \"" + (stages[idx] ? stages[idx].name : work.current_stage) + "\"", "error"); return; }
  var next = nextIds.length === 1 ? nextIds[0] : prompt("Move to stage: " + nextIds.join(", "));
  if (!next) return;
  try {
    var r = await _xhrFetch(BASE + "/works/" + workId + "/complete", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({agent_id: "", confidence: 1.0, action: "move to " + next, next_stage: next})
    });
    if (!r.ok) { var e = await r.json().catch(function(){ return {}; }); flash("Move failed: " + (e.error || r.statusText), "error"); return; }
    load();
  } catch(e) { flash("Network error: " + e.message, "error"); console.error(e); }
}

// ── Work Detail Modal ─────────────────────────────────────────

var _detailWorkId = "";
var _detailWork = null;

function openWorkDetail(workId) {
  _detailWorkId = workId;
  _detailWork = (_cachedWorks || []).find(function(w) { return w.id === workId; });
  if (!_detailWork) return;

  var stage = (_cachedStages || []).find(function(s) { return s.id === _detailWork.current_stage; });
  document.getElementById("wdTitle").textContent = _detailWork.title;
  document.getElementById("wdStageBadge").textContent = stage ? stage.name : _detailWork.current_stage;
  document.getElementById("wdMeta").textContent = (_detailWork.priority || "normal") +
    " · created " + (_detailWork.created_at || "").slice(0, 10);
  var ae = document.getElementById("wdAssignee");
  if (_detailWork.assignee) {
    ae.textContent = "Assigned to " + _detailWork.assignee;
    ae.className = "wd-assignee assigned";
  } else {
    ae.textContent = "";
    ae.className = "wd-assignee";
  }
  resetAssign();
  document.getElementById("wdMessages").innerHTML = '<div class="wd-empty">Loading work context...</div>';
  // Populate stage dropdown with all stages
  var sel = document.getElementById("wdStageSelect");
  var currentStageId = _detailWork.current_stage || "";
  var opts = "";
  for (var i = 0; i < _cachedStages.length; i++) {
    var s = _cachedStages[i];
    var selected = s.id === currentStageId ? " selected" : "";
    opts += '<option value="' + esc(s.id) + '"' + selected + '>' + esc(s.name || s.id) + '</option>';
  }
  sel.innerHTML = opts;
  sel.disabled = false;

  // Populate output type and description
  document.getElementById("wdOutputType").value = _detailWork.output_type || "";
  document.getElementById("wdOutputDesc").value = _detailWork.output_description || "";
  // Collapse output details by default (unless output is set)
  var outputDetails = document.getElementById("wdOutputDetails");
  outputDetails.open = !!(_detailWork.output_type || _detailWork.output_description);

  var ci = document.getElementById("wdCommentInput");
  ci.value = "";
  document.getElementById("workDetailModal").classList.add("open");
  loadWorkContext(workId);
  updateActButton();
  setTimeout(function(){ ci.focus(); }, 150);
}

function closeWorkDetail() {
  document.getElementById("workDetailModal").classList.remove("open");
  _detailWorkId = "";
  _detailWork = null;
  stopActPolling();  // clean up any active Act status polling
}

async function loadWorkContext(workId) {
  try {
    var r = await _xhrFetch(BASE + "/works/" + workId + "/context");
    if (!r.ok) {
      document.getElementById("wdMessages").innerHTML = '<div class="wd-empty">Failed to load context</div>';
      return;
    }
    var data = await r.json();
    if (!data.events || data.events.length === 0) {
      document.getElementById("wdMessages").innerHTML = '<div class="wd-empty">No context history yet</div>';
    } else {
      renderWorkContext(data.events);
    }
  } catch(e) {
    document.getElementById("wdMessages").innerHTML = '<div class="wd-empty">Failed to load context</div>';
    console.error(e);
  }
}

function renderWorkContext(events) {
  var html = "";
  for (var i = 0; i < events.length; i++) {
    var ev = events[i];
    var t = ev.type || "unknown";
    var ts = (ev.ts || "").replace("T", " ").slice(0, 19);
    var cls = "";
    var label = "";
    var body = "";

    if (t === "created") {
      cls = "wd-msg-system";
      body = "Created &ldquo;" + esc(ev.title || "") + "&rdquo; in <b>" + esc(ev.stage || "") + "</b>" +
             (ev.creator ? " by " + esc(ev.creator) : "");
    } else if (t === "assigned") {
      cls = "wd-msg-system";
      body = "Assigned to <b>" + esc(ev.agent_id || "") + "</b>" +
             (ev.strategy ? " (strategy: " + esc(ev.strategy) + ")" : "");
    } else if (t === "stage_changed") {
      cls = "wd-msg-system";
      body = "Moved from <b>" + esc(ev["from"] || "") + "</b> &rarr; <b>" + esc(ev.to || "") + "</b>" +
             (ev.reason ? " (" + esc(ev.reason) + ")" : "");
    } else if (t === "completed") {
      cls = "wd-msg-system";
      var pct = ev.confidence != null ? (ev.confidence * 100).toFixed(0) + "%" : "";
      body = "Stage completed" + (pct ? " (confidence: " + pct + ")" : "") +
             (ev.next_stage ? " &rarr; " + esc(ev.next_stage) : "");
    } else if (t === "act_triggered") {
      cls = "wd-msg-system";
      body = "⚡ Act! Triggered <b>" + esc(ev.agent_id || "agent") + "</b>" +
             " to process this work item" +
             (ev.triggered_by ? " (by " + esc(ev.triggered_by) + ")" : "");
    } else if (t === "failed") {
      cls = "wd-msg-system";
      body = "Failed: " + esc(ev.error || "unknown error") +
             (ev.retryable ? " (retryable)" : "");
    } else if (t === "safety_triggered") {
      cls = "wd-msg-system";
      body = "Safety gate triggered: " + esc(ev.reason || ev.action || "");
    } else if (t === "safety_resolved") {
      cls = "wd-msg-system";
      body = "Safety resolved: " + esc(ev.decision || "") + " by " + esc(ev.decided_by || "");
    } else if (t === "context_update") {
      cls = "wd-msg-system";
      body = esc(ev.key || "") + " = " + esc((ev.value || "").toString());
    } else if (t === "step_complete") {
      cls = "wd-msg-system";
      var icon = ev.success ? "✓" : "✗";
      body = icon + " Step " + ((ev.step_index || 0) + 1) + "/" + (ev.total_steps || 0) +
             ": " + esc(ev.summary || "");
    } else if (t === "thought") {
      cls = "wd-msg-agent";
      label = "thought";
      body = esc(ev.content || "");
    } else if (t === "tool_call") {
      cls = "wd-msg-agent";
      label = "tool: " + esc(ev.tool || "unknown");
      body = esc(typeof ev.input === "string" ? ev.input : JSON.stringify(ev.input || {}));
    } else if (t === "response") {
      cls = "wd-msg-agent";
      label = "response";
      body = esc(ev.content || "");
    } else if (t === "human_direction") {
      cls = "wd-msg-human";
      label = esc(ev.human_id || "human");
      body = esc(ev.content || "");
    } else if (t === "comment") {
      cls = "wd-msg-human";
      label = esc(ev.author || "User");
      body = esc(ev.content || "");
    } else if (t === "output_updated") {
      cls = "wd-msg-system";
      body = "Output updated: " + esc(ev.output_type || "type cleared") +
             (ev.output_description ? " — " + esc(ev.output_description.substring(0, 80)) : "");
    } else {
      cls = "wd-msg-system";
      body = "[" + esc(t) + "] " + esc(JSON.stringify(ev).substring(0, 200));
    }

    html += '<div class="' + cls + '">';
    if (label) html += '<span class="wd-msg-label">' + label + '</span>';
    html += body;
    html += '<span class="wd-msg-ts">' + esc(ts) + '</span>';
    html += '</div>';
  }
  document.getElementById("wdMessages").innerHTML = html;
  var el = document.getElementById("wdMessages");
  el.scrollTop = el.scrollHeight;
}

async function addWorkComment() {
  var input = document.getElementById("wdCommentInput");
  var content = input.value.trim();
  if (!content || !_detailWorkId) return;
  try {
    var r = await _xhrFetch(BASE + "/works/" + _detailWorkId + "/comment", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({content: content, author: "User"})
    });
    if (!r.ok) { var e = await r.json().catch(function(){ return {}; }); flash("Comment failed: " + (e.error || r.statusText), "error"); return; }
    input.value = "";
    loadWorkContext(_detailWorkId);
  } catch(e) { flash("Network error: " + e.message, "error"); }
}

function stageSelectChanged() {
  var wid = _detailWorkId;
  var newStage = document.getElementById("wdStageSelect").value;
  if (wid && newStage && _detailWork && newStage !== _detailWork.current_stage) {
    moveWorkToStage(wid, newStage);
    // Update the modal's stage badge instantly
    var ns = (_cachedStages || []).find(function(s) { return s.id === newStage; });
    document.getElementById("wdStageBadge").textContent = ns ? ns.name : newStage;
    _detailWork.current_stage = newStage;
  }
}

async function openAssign() {
  var btn = document.getElementById("wdAssignBtn");
  var sel = document.getElementById("wdAssignSelect");
  var currentAssignee = (_detailWork && _detailWork.assignee) ? _detailWork.assignee : "";
  btn.style.display = "none";
  sel.style.display = "";
  sel.innerHTML = '<option value="">Loading...</option>';
  sel.focus();
  try {
    var r = await _xhrFetch(BASE + "/agents");
    var agents = await r.json();
    if (!Array.isArray(agents) || agents.length === 0) {
      sel.innerHTML = '<option value="">No agents available</option>';
      return;
    }
    var opts = '<option value="">Assign...</option>';
    var found = false;
    for (var i = 0; i < agents.length; i++) {
      var a = agents[i];
      var name = a.name || a.id || a.agent_id || ("Agent " + i);
      var id = a.id || a.agent_id || name;
      var selAttr = (id === currentAssignee) ? " selected" : "";
      if (id === currentAssignee) found = true;
      opts += '<option value="' + esc(id) + '"' + selAttr + '>' + esc(name) + '</option>';
    }
    // If current assignee is not in the agent list, add them
    if (currentAssignee && !found) {
      opts += '<option value="' + esc(currentAssignee) + '" selected>' + esc(currentAssignee) + '</option>';
    }
    sel.innerHTML = opts;
  } catch(e) {
    sel.innerHTML = '<option value="">Failed to load agents</option>';
    console.error(e);
  }
}

function resetAssign() {
  document.getElementById("wdAssignSelect").style.display = "none";
  document.getElementById("wdAssignBtn").style.display = "";
}

async function onAssignSelect(agentId) {
  if (!agentId || !_detailWorkId) { resetAssign(); return; }
  try {
    var r = await _xhrFetch(BASE + "/works/" + _detailWorkId + "/assign", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({agent_id: agentId, stage_id: _detailWork ? _detailWork.current_stage : "", reason: "Manual assignment from kanban"})
    });
    if (!r.ok) { var e = await r.json().catch(function(){ return {}; }); flash("Assign failed: " + (e.error || r.statusText), "error"); resetAssign(); return; }
    flash("Assigned to " + agentId, "success");
    resetAssign();
    load();
    closeWorkDetail();
  } catch(e) { flash("Network error: " + e.message, "error"); resetAssign(); }
}

async function moveWorkToStage(workId, targetStage) {
  if (!workId || !targetStage) return;
  var stageNames = {};
  (_cachedStages || []).forEach(function(s) { stageNames[s.id] = s.name || s.id; });
  try {
    var r = await _xhrFetch(BASE + "/works/" + workId + "/complete", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({agent_id: "", confidence: 1.0, action: "move to " + targetStage, next_stage: targetStage})
    });
    if (!r.ok) { var e = await r.json().catch(function(){ return {}; }); flash("Move failed: " + (e.error || r.statusText), "error"); return; }
    var data = await r.json();
    flash("Moved to " + (stageNames[targetStage] || targetStage) + " (stage=" + (data.new_stage || targetStage) + ")", "success");
    load();
  } catch(e) { flash("Network error: " + e.message, "error"); console.error(e); }
}

// ── Edit Project ──────────────────────────────────────────────

var _editStages = [];

function openEditProject() {
  document.getElementById("editProjName").value = document.querySelector(".top-bar h1").textContent;
  document.getElementById("editProjDesc").value = "";
  document.getElementById("editProjKey").value = PROJECT;
  document.getElementById("editProjectModal").classList.add("open");
}

function closeEditProject() {
  document.getElementById("editProjectModal").classList.remove("open");
}

async function saveProject() {
  var name = document.getElementById("editProjName").value.trim();
  var desc = document.getElementById("editProjDesc").value.trim();
  var newKey = document.getElementById("editProjKey").value.trim();
  if (!name || !newKey) return;

  try {
    var r = await _xhrFetch(BASE + "/update", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({project_name: name, description: desc, project_key: newKey})
    });
    if (!r.ok) { var e = await r.json(); throw new Error(e.error || "Save failed"); }
    var data = await r.json();
    closeEditProject();
    flash("Project updated", "success");
    if (data.redirect) {
      setTimeout(function(){ window.location.href = data.redirect; }, 600);
    } else {
      setTimeout(function(){ window.location.reload(); }, 600);
    }
  } catch(e) { flash(e.message, "error"); }
}

// ── Edit Board ────────────────────────────────────────────────

function openEditBoard() {
  _editStages = JSON.parse(JSON.stringify(_cachedStages));
  renderStageEditor();
  closeImportPanel();
  var hasWorks = _cachedWorks && _cachedWorks.length > 0;
  document.getElementById("importBoardBtn").style.display = hasWorks ? "none" : "block";
  document.getElementById("editBoardModal").classList.add("open");
}

function closeEditBoard() {
  document.getElementById("editBoardModal").classList.remove("open");
}

function addStageRow() {
  var id = "stage-" + Date.now();
  _editStages.push({id: id, name: "", allowed_next: []});
  renderStageEditor();
}

function removeStageRow(idx) {
  _editStages.splice(idx, 1);
  renderStageEditor();
}

function renderStageEditor() {
  var html = "";
  for (var i = 0; i < _editStages.length; i++) {
    var s = _editStages[i];
    html += '<div class="stage-row">' +
            '<input class="stage-id" value="' + esc(s.id) + '" onchange="_editStages[' + i + '].id=this.value;refreshInitialSelect()" placeholder="id">' +
            '<input class="stage-name" value="' + esc(s.name) + '" onchange="_editStages[' + i + '].name=this.value" placeholder="Name">' +
            '<input class="allowed-next" value="' + esc((s.allowed_next||[]).join(", ")) + '" onchange="_editStages[' + i + '].allowed_next=this.value.split(\',\').map(function(x){return x.trim()}).filter(Boolean)" placeholder="Next stages (comma)">' +
            '<button class="danger" onclick="removeStageRow(' + i + ')">&times;</button>' +
            '</div>';
  }
  if (!html) html = '<div class="empty-state">No stages. Add at least one.</div>';
  document.getElementById("stageList").innerHTML = html;
  refreshInitialSelect();
}

function refreshInitialSelect() {
  var sel = document.getElementById("editInitialStage");
  var cur = sel.value;
  var opts = "";
  for (var j = 0; j < _editStages.length; j++) {
    var sid = _editStages[j].id;
    var selAttr = (j === 0 && !cur) ? " selected" : (sid === cur ? " selected" : "");
    opts += '<option value="' + esc(sid) + '"' + selAttr + '>' + esc(sid) + '</option>';
  }
  sel.innerHTML = opts;
}

async function saveStages() {
  if (_editStages.length === 0) {
    flash("At least one stage is required", "error");
    return;
  }
  var initialStage = document.getElementById("editInitialStage").value || _editStages[0].id;

  try {
    var r = await _xhrFetch(BASE + "/stages/update", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({stages: _editStages, initial_stage: initialStage})
    });
    if (!r.ok) { var e = await r.json(); throw new Error(e.error || "Save failed"); }
    closeEditBoard();
    flash("Board stages updated", "success");
    load();
  } catch(e) { flash(e.message, "error"); }
}

// ── Import Board ──────────────────────────────────────────────

var _selectedImportKey = "";
var _selectedImportName = "";

async function openImportPanel() {
  var panel = document.getElementById("importBoardSection");
  var list = document.getElementById("importProjectList");
  _selectedImportKey = "";
  _selectedImportName = "";
  list.innerHTML = '<div style="text-align:center;color:#555;padding:12px;font-size:12px;">Loading projects...</div>';
  panel.style.display = "block";

  try {
    var r = await _xhrFetch("/api/v1/team/projects");
    if (!r.ok) throw new Error("Failed to fetch projects");
    var data = await r.json();
    var projects = (data.projects || []).filter(function(p) { return p.project_key !== PROJECT; });

    if (projects.length === 0) {
      list.innerHTML = '<div style="text-align:center;color:#555;padding:12px;font-size:12px;">No other projects found</div>';
      return;
    }

    var html = "";
    for (var i = 0; i < projects.length; i++) {
      var p = projects[i];
      var stageNames = (p.stages || []).map(function(s) { return s.name || s.id; }).join(" → ");
      html += '<div class="import-select-card" data-key="' + esc(p.project_key) + '" data-name="' + esc(p.project_name) + '" ' +
              'style="padding:10px 12px;margin-bottom:6px;background:#13151f;border:1px solid #1e2030;border-radius:6px;cursor:pointer;" ' +
              'onclick="selectImportProject(this,\'' + esc(p.project_key) + '\',\'' + esc(p.project_name) + '\')">' +
              '<div style="font-size:13px;font-weight:500;">' + esc(p.project_name) + '</div>' +
              '<div style="font-size:11px;color:#666;margin-top:2px;">' + esc(p.project_key) + ' &middot; ' + p.stage_count + ' stages &middot; ' + esc(stageNames) + '</div>' +
              '</div>';
    }
    html += '<div style="margin-top:8px;display:flex;gap:8px;justify-content:flex-end;">' +
            '<button id="importConfirmBtn" class="bar-btn" style="background:#6366f1;border-color:#6366f1;opacity:0.4;cursor:not-allowed;" disabled onclick="executeImport()">Import Selected</button>' +
            '</div>';
    list.innerHTML = html;
  } catch(e) {
    list.innerHTML = '<div style="text-align:center;color:#fca5a5;padding:12px;font-size:12px;">Failed to load projects</div>';
  }
}

function selectImportProject(el, key, name) {
  _selectedImportKey = key;
  _selectedImportName = name;
  var cards = document.querySelectorAll("#importProjectList .import-select-card");
  for (var i = 0; i < cards.length; i++) {
    cards[i].style.borderColor = "#1e2030";
    cards[i].style.background = "#13151f";
  }
  el.style.borderColor = "#6366f1";
  el.style.background = "#1a1d2b";
  var btn = document.getElementById("importConfirmBtn");
  if (btn) {
    btn.disabled = false;
    btn.style.opacity = "1";
    btn.style.cursor = "pointer";
    btn.textContent = "Import \"" + name + "\"";
  }
}

function closeImportPanel() {
  _selectedImportKey = "";
  _selectedImportName = "";
  document.getElementById("importBoardSection").style.display = "none";
}

async function executeImport() {
  if (!_selectedImportKey) return;

  try {
    var r = await _xhrFetch(BASE + "/boards/import", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({source_project_key: _selectedImportKey})
    });
    var data = await r.json();
    if (!r.ok) throw new Error(data.error || "Import failed");

    closeImportPanel();
    closeEditBoard();
    _selectedImportKey = "";
    _selectedImportName = "";
    window.location.reload();
  } catch(e) { flash(e.message, "error"); }
}

// ── Project / Output Directory ────────────────────────────────

async function openProjectDir() {
  try {
    var r = await _xhrFetch(BASE + "/open-dir", { method: "POST" });
    var data = await r.json();
    flash("Opened: " + data.path, "success");
  } catch(e) { flash("Failed: " + e.message, "error"); }
}

async function openOutputDir() {
  if (!_detailWorkId) return;
  try {
    var r = await _xhrFetch(BASE + "/works/" + _detailWorkId + "/open-output-dir", { method: "POST" });
    var data = await r.json();
    flash("Opened: " + data.path + (data.files ? " (" + data.files.length + " files)" : ""), "success");
  } catch(e) { flash("Failed: " + e.message, "error"); }
}

async function saveWorkOutput() {
  if (!_detailWorkId) return;
  var outputType = document.getElementById("wdOutputType").value;
  var outputDesc = document.getElementById("wdOutputDesc").value.trim();
  try {
    var r = await _xhrFetch(BASE + "/works/" + _detailWorkId + "/output", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({output_type: outputType, output_description: outputDesc})
    });
    if (!r.ok) { var e = await r.json().catch(function(){ return {}; }); flash("Save failed: " + (e.error || r.statusText), "error"); return; }
    var data = await r.json();
    if (_detailWork) {
      _detailWork.output_type = outputType;
      _detailWork.output_description = outputDesc;
    }
    flash("Output info saved", "success");
    load();
  } catch(e) { flash("Network error: " + e.message, "error"); }
}


var _agentStatuses = {};  // agent_id → {status, system_state, activity}
var _actTriggeredAgent = null;  // agent_id that was just triggered via Act
var _actPoller = null;  // interval handle for polling agent status after Act

async function fetchAgentInfo(agentId) {
  try {
    var r = await _xhrFetch(BASE + "/agents");
    if (!r.ok) return null;
    var agents = await r.json();
    if (Array.isArray(agents)) {
      for (var i = 0; i < agents.length; i++) {
        var a = agents[i];
        var id = a.id || a.name;
        _agentStatuses[id] = {
          status: a.status,
          system_state: a.system_state || "",
          activity: a.activity || ""
        };
        if (id === agentId) return _agentStatuses[id];
      }
    }
  } catch(e) { /* ignore */ }
  return null;
}

function agentStatusText(info) {
  // Build a human-readable status line from the agent info.
  if (!info) return "Unknown";
  var parts = [info.status];
  if (info.system_state && info.system_state !== "Idle") {
    parts.push("· " + info.system_state);
  }
  if (info.activity) {
    parts.push("· " + info.activity);
  }
  return parts.join(" ");
}

async function updateActButton() {
  var btn = document.getElementById("wdActBtn");
  if (!btn) return;

  // While an Act-triggered agent is processing, keep button disabled.
  if (_actTriggeredAgent && _actPoller) {
    btn.disabled = true;
    var info = _agentStatuses[_actTriggeredAgent];
    btn.title = "Agent " + _actTriggeredAgent + " is processing — " + agentStatusText(info || {status: "Busy"});
    // Update button text to show current activity
    if (info && info.activity) {
      btn.textContent = "⚡ " + info.activity;
    }
    return;
  }

  // Disable if no assignee or work is already marked for review
  var assignee = _detailWork && _detailWork.assignee;
  if (!assignee) {
    btn.disabled = true;
    btn.title = "No agent assigned";
    return;
  }

  // Fetch agent statuses if not already cached
  if (!Object.keys(_agentStatuses).length) {
    var info = await fetchAgentInfo(assignee);
    if (info === null) { btn.disabled = true; btn.title = "Failed to fetch agent status"; return; }
  }

  var info = _agentStatuses[assignee];
  var status = info ? info.status : "unknown";
  if (status === "Idle") {
    btn.disabled = false;
    btn.textContent = "⚡ Act!";
    btn.title = "Trigger " + assignee + " to process this work item now";
  } else {
    btn.disabled = true;
    btn.title = "Agent " + assignee + " — " + agentStatusText(info || {status: status});
  }
}

function startActPolling(agentId) {
  _actTriggeredAgent = agentId;
  var btn = document.getElementById("wdActBtn");
  btn.disabled = true;
  btn.textContent = "⚡ Processing...";
  btn.title = "Agent " + agentId + " is processing — waiting for idle";

  // Poll every 3s until agent leaves Idle, then continue until it returns to Idle
  var seenWorking = false;
  _actPoller = setInterval(async function() {
    var info = await fetchAgentInfo(agentId);
    if (info === null) return;  // network error, keep polling

    if (!seenWorking && info.status !== "Idle") {
      // Agent started working!
      seenWorking = true;
      if (btn) {
        btn.textContent = "⚡ " + (info.activity || "Working...");
        btn.title = agentId + " — " + agentStatusText(info);
      }
    }

    if (seenWorking && info.activity && btn) {
      // Update button text to reflect current activity in real time
      btn.textContent = "⚡ " + info.activity;
      btn.title = agentId + " — " + agentStatusText(info);
    }

    if (seenWorking && info.status === "Idle") {
      // Agent finished and returned to idle — re-enable the button
      stopActPolling();
      if (btn) { btn.textContent = "⚡ Act!"; btn.title = "Trigger agent to process this work item now"; }
      flash("Agent " + agentId + " finished processing", "success");
      // Refresh work detail to show any updates
      if (_detailWorkId) openWorkDetail(_detailWorkId);
    }
  }, 3000);
}

function stopActPolling() {
  _actTriggeredAgent = null;
  if (_actPoller) { clearInterval(_actPoller); _actPoller = null; }
}

async function actOnWork() {
  if (!_detailWorkId) return;
  var btn = document.getElementById("wdActBtn");
  btn.disabled = true;
  btn.textContent = "⚡ Acting...";
  try {
    var r = await _xhrFetch(BASE + "/works/" + _detailWorkId + "/act", {
      method: "POST",
      headers: {"Content-Type": "application/json"},
      body: JSON.stringify({triggered_by: "User"})
    });
    var data = await r.json();
    if (!r.ok) {
      flash(data.error || "Act failed", "error");
      btn.textContent = "⚡ Act!";
      updateActButton();
      return;
    }
    // Invalidate cache and start polling — button stays disabled until agent
    // leaves Idle and returns to Idle (processing cycle complete).
    _agentStatuses = {};
    var agentId = data.agent_id;
    flash("Triggered " + (agentId || "agent") + " — waiting for agent to start processing...", "success");
    btn.textContent = "⚡ Awaiting...";
    startActPolling(agentId);
  } catch(e) {
    flash("Network error: " + e.message, "error");
    btn.textContent = "⚡ Act!";
    updateActButton();
  }
}

// ── Helpers ───────────────────────────────────────────────────

function esc(s) { return (s||"").replace(/&/g,"&amp;").replace(/</g,"&lt;").replace(/>/g,"&gt;").replace(/"/g,"&quot;"); }

function flash(msg, type) {
  var el = document.getElementById("flash");
  el.className = "flash " + (type || "success");
  el.textContent = msg;
  setTimeout(function(){ el.className = ""; el.textContent = ""; }, 3500);
}

// ── Init ──────────────────────────────────────────────────────

document.addEventListener("keydown", function(e) {
  if (e.key === "Escape") {
    var sel = document.getElementById("wdAssignSelect");
    if (sel && sel.style.display !== "none") {
      resetAssign();
      return;
    }
    var modal = document.getElementById("workDetailModal");
    if (modal && modal.classList.contains("open")) {
      closeWorkDetail();
    }
  }
});

// Wire chat-input send event to comment function and load skills for autocomplete
(function() {
  var ci = document.getElementById("wdCommentInput");
  if (!ci) return;
  ci.addEventListener("send", function() { addWorkComment(); });

  // Load skills for /skill autocomplete dropdown
  _xhrFetch("/api/v1/llm-skills")
    .then(function(r) {
      if (!r.ok) return;
      return r.json();
    })
    .then(function(data) {
      var items = (data && data.items) || [];
      var skills = items.filter(function(s) { return s.name && s.description; });
      ci.skills = skills;
    })
    .catch(function() { /* Skills unavailable — picker stays empty */ });
})();

load();
setInterval(load, 10000);

// Notify parent app of current URL so it can restore this page on revisit
if (window.parent !== window) {
  window.parent.postMessage({type: "aman:team-url", url: window.location.pathname}, "*");
}
