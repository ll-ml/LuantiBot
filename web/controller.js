const baseUrlInput = document.getElementById("baseUrl");
const authTokenInput = document.getElementById("authToken");
const outputEl = document.getElementById("output");
const metaEl = document.getElementById("requestMeta");
const statusEl = document.getElementById("connectionStatus");
const clearBtn = document.getElementById("clearBtn");
const pingBtn = document.getElementById("pingBtn");

const formEls = Array.from(document.querySelectorAll(".action-form"));

function buildQuery(form) {
  const params = new URLSearchParams();
  for (const field of form.elements) {
    if (!field.name) continue;
    const value = field.value.trim();
    if (value === "") continue;
    params.append(field.name, value);
  }
  const query = params.toString();
  return query ? `?${query}` : "";
}

function buildHeaders() {
  const headers = new Headers();
  const token = authTokenInput.value.trim();
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }
  return headers;
}

function buildJsonBody(form) {
  const payload = {};
  for (const field of form.elements) {
    if (!field.name) continue;
    const value = field.value.trim();
    if (value === "") continue;
    payload[field.name] = value;
  }
  return JSON.stringify(payload);
}

function formatResponse(text) {
  try {
    const json = JSON.parse(text);
    return { formatted: JSON.stringify(json, null, 2), isJson: true };
  } catch (error) {
    return { formatted: text || "(empty response)", isJson: false };
  }
}

function escapeHtml(value) {
  return value
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

function highlightJson(jsonText) {
  const escaped = escapeHtml(jsonText);
  return escaped.replace(
    /("(?:\\u[a-fA-F0-9]{4}|\\[^u]|[^\\"])*"\s*:)|("(?:\\u[a-fA-F0-9]{4}|\\[^u]|[^\\"])*")|\b(true|false|null)\b|\b-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?\b/g,
    (match, key, stringValue, literal) => {
      if (key) return `<span class="json-key">${key}</span>`;
      if (stringValue) return `<span class="json-string">${stringValue}</span>`;
      if (literal) return `<span class="json-literal">${literal}</span>`;
      return `<span class="json-number">${match}</span>`;
    }
  );
}

function setMeta(status, method, url, elapsedMs) {
  const stamp = new Date().toLocaleTimeString();
  metaEl.textContent = `${status} • ${method} ${url} • ${elapsedMs}ms • ${stamp}`;
}

async function sendRequest(endpoint, method, query, body) {
  const base = baseUrlInput.value.trim().replace(/\/$/, "");
  const url = `${base}${endpoint}${query || ""}`;
  const headers = buildHeaders();
  const options = { method, headers };
  if (body != null) {
    headers.set("Content-Type", "application/json");
    options.body = body;
  }
  const started = performance.now();

  try {
    const response = await fetch(url, options);
    const text = await response.text();
    const elapsed = Math.round(performance.now() - started);
    const { formatted, isJson } = formatResponse(text);

    if (isJson) {
      outputEl.innerHTML = highlightJson(formatted);
      outputEl.classList.add("is-json");
    } else {
      outputEl.textContent = formatted;
      outputEl.classList.remove("is-json");
    }
    setMeta(`${response.status} ${response.statusText}`, method, url, elapsed);
    statusEl.textContent = response.ok ? "Connected" : "Error";
    statusEl.style.background = response.ok
      ? "rgba(74, 155, 94, 0.14)"
      : "rgba(203, 91, 43, 0.18)";
    statusEl.style.color = response.ok ? "#2a6b3f" : "#cb5b2b";
  } catch (error) {
    const elapsed = Math.round(performance.now() - started);
    outputEl.textContent = `Request failed: ${error.message}`;
    outputEl.classList.remove("is-json");
    setMeta("Network error", method, url, elapsed);
    statusEl.textContent = "Disconnected";
    statusEl.style.background = "rgba(203, 91, 43, 0.18)";
    statusEl.style.color = "#cb5b2b";
  }
}

formEls.forEach((form) => {
  form.addEventListener("submit", (event) => {
    event.preventDefault();
    const endpoint = form.dataset.endpoint;
    const method = form.dataset.method || "GET";
    const jsonEndpoints = new Set(["/sleep", "/chat", "/say"]);
    if (method.toUpperCase() === "POST" && jsonEndpoints.has(endpoint)) {
      const body = buildJsonBody(form);
      sendRequest(endpoint, method, "", body);
    } else {
      const query = buildQuery(form);
      sendRequest(endpoint, method, query);
    }
  });
});

clearBtn.addEventListener("click", () => {
  outputEl.textContent = "Waiting for a request...";
  outputEl.classList.remove("is-json");
  metaEl.textContent = "No requests yet.";
});

pingBtn.addEventListener("click", () => {
  sendRequest("/health", "GET", "");
});
