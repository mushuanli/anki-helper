'use strict';
const $ = (id) => document.getElementById(id);
let token = location.hash.slice(1);
if (token) sessionStorage.setItem('helper-token', token);
else token = sessionStorage.getItem('helper-token') || '';
history.replaceState(null, '', '/');
let catalog = null, settingsDraft = null, testRevision = 0;
const purposes = {
  vision: { connection: 'vision_connection', model: 'vision_model', preset: 'vision-preset', catalog: 'vision', custom: 'vision-custom' },
  text: { connection: 'text_connection', model: 'openai_model', preset: 'text-preset', catalog: 'text', custom: 'text-custom' },
  image: { connection: 'image_connection', model: 'image_model', preset: 'image-preset', catalog: 'image', custom: 'image-custom' },
  audio: { connection: 'tts_connection', model: 'tts_model', preset: 'tts-preset', catalog: 'tts', custom: 'tts-custom' },
};
const supports = (connection, kind) =>
  connection?.provider === 'ark' && ['text', 'image', 'vision'].includes(kind) || connection?.provider === 'doubao' && kind === 'audio';
let mediaSettings = { audio_enabled: false, images_enabled: false };
let projects = [], current = null, cards = [], selected = null, dirty = false, busy = false;
let generation = 0, polling = null, jobStarted = 0, lastJob = null, toastTimer = null;
const fields = ['symbol', 'chn', 'example_en', 'example_cn', 'word_family', 'collocations', 'memory_tips', 'image_prompt'];
function notice(message, error = false) {
  $('notice').textContent = message; $('notice').classList.toggle('error', error); $('notice').hidden = false;
  clearTimeout(toastTimer); toastTimer = setTimeout(() => { $('notice').hidden = true; }, error ? 10000 : 4500);
}
async function api(path, options = {}) {
  const response = await fetch('/api' + path, { ...options, headers: { 'Content-Type': 'application/json', 'X-Helper-Token': token, ...options.headers } });
  if (!response.ok) {
    let error = '请求失败，请检查应用是否仍在运行。';
    try { error = (await response.json()).error || error; } catch (_) { if (response.status === 403) error = '页面会话失效，请从应用打印的地址重新打开。'; }
    throw new Error(error);
  }
  return response.json();
}
const jsonOptions = (method, data) => ({ method, body: JSON.stringify(data) });
function safely(action) { return async (event) => { try { await action(event); } catch (error) { notice(error.message, true); } }; }
function projectApi(suffix = '') { return '/projects/' + encodeURIComponent(current.id) + suffix; }
function node(tag, text, className) { const element = document.createElement(tag); if (text !== undefined) element.textContent = text; if (className) element.className = className; return element; }
function hasContent(card) { return fields.every(field => card.data[field]?.trim()); }
function cardStatus(card) {
  const status = [hasContent(card) ? '文本完整' : '文本待补全'];
  status.push(card.media_status?.audio && card.media_status?.audio_example ? '音频就绪' : mediaSettings.audio_enabled ? '缺音频' : '音频未启用');
  status.push(card.media_status?.image ? '配图就绪' : mediaSettings.images_enabled ? '缺配图' : '配图未启用');
  if (lastJob?.failed_cards?.includes(card.id)) status.push('生成失败');
  return status.join(' · ');
}
async function loadProjects() {
  const [data, config] = await Promise.all([api('/projects'), api('/config')]); projects = data.projects; mediaSettings = config;
  $('runtime-dir').textContent = data.runtime_dir; renderProjects();
}
function renderProjects() {
  $('projects').replaceChildren();
  for (const project of projects) {
    const button = node('button', project.name, 'project-item' + (project.id === current?.id ? ' active' : ''));
    button.addEventListener('click', safely(() => selectProject(project))); $('projects').append(button);
  }
}
async function selectProject(project) {
  if (dirty && !confirm('当前卡片有未保存的修改，放弃修改并切换项目？')) return;
  stopPolling(); jobStarted = 0; const ticket = ++generation;
  const data = await api('/projects/' + encodeURIComponent(project.id));
  if (ticket !== generation) return;
  current = data.project; cards = data.cards; selected = null; dirty = false; busy = false; lastJob = null;
  $('welcome').hidden = true; $('workspace').hidden = false; $('project-name').textContent = current.name;
  $('project-path').textContent = current.path; $('job-panel').hidden = true;
  $('search').value = ''; renderProjects(); renderCards(); renderEditor();
  if (cards.length) chooseCard(cards[0]);
  await pollJob(ticket);
}
function renderCards() {
  const query = $('search').value.trim().toLowerCase(); $('cards').replaceChildren();
  $('card-count').textContent = `${cards.length} 张卡片 · ${cards.filter(hasContent).length} 张文本完整`;
  const visible = cards.filter(c => (c.data.name + ' ' + (c.data.chn || '')).toLowerCase().includes(query));
  for (const card of visible) {
    const button = node('button', undefined, 'card-row' + (card.id === selected?.id ? ' active' : ''));
    const info = node('span'); info.append(node('b', card.data.name || card.id), node('small', card.data.chn || '等待生成'), node('small', cardStatus(card)));
    button.append(info, node('span', hasContent(card) ? '可编辑' : '待生成', 'badge' + (hasContent(card) ? ' ready' : '')));
    button.addEventListener('click', () => chooseCard(card)); $('cards').append(button);
  }
  if (!visible.length) $('cards').append(node('p', cards.length ? '没有匹配的单词。' : '此目录还没有单词 JSON。可创建新项目并导入单词。', 'empty'));
}
function chooseCard(card) {
  if (card.id === selected?.id) return;
  if (dirty && !confirm('当前卡片有未保存的修改，放弃修改并切换卡片？')) return;
  selected = card; dirty = false; renderCards(); renderEditor();
}
function renderEditor() {
  $('editor').hidden = !selected; $('editor-empty').hidden = Boolean(selected);
  if (!selected) return;
  $('word-name').textContent = selected.data.name || selected.id;
  for (const field of fields) $('card-form').elements[field].value = selected.data[field] || '';
  $('dirty-note').textContent = ''; renderMedia(); setBusy(busy);
}
function renderMedia() {
  $('media-preview').replaceChildren(); if (!selected) return;
  const data = selected.data;
  const url = (dir, name) => '/api' + projectApi('/media/' + dir + '/' + encodeURIComponent(name));
  if (data.image) {
    const img = node('img'); img.src = url('images', data.image); img.alt = `${data.name} 的配图`; img.loading = 'lazy';
    img.addEventListener('error', () => { img.replaceWith(node('p', '配图文件缺失，可生成 / 补全此卡重试。')); });
    $('media-preview').append(img);
  }
  for (const [field, label] of [['audio', '单词发音'], ['audio_example', '例句朗读']]) {
    if (!data[field]) continue;
    const wrapper = node('div'); const audio = node('audio'); audio.controls = true; audio.preload = 'none'; audio.src = url('audio', data[field]);
    audio.addEventListener('error', () => { wrapper.replaceChildren(node('p', `${label}文件缺失，可生成 / 补全此卡重试。`)); });
    wrapper.append(node('p', label), audio); $('media-preview').append(wrapper);
  }
  if (!data.image && !data.audio && !data.audio_example) $('media-preview').append(node('p', '在服务设置中启用图片或音频，即可生成媒体预览。'));
}
function setBusy(value) {
  busy = value;
  for (const id of ['generate-all', 'generate-one', 'regenerate-one', 'regenerate-scope', 'retry-failed', 'export-apkg', 'export-md', 'save-card', 'import-button']) $(id).disabled = value;
  for (const field of fields) $('card-form').elements[field].disabled = value;
}
function stopPolling() { clearTimeout(polling); polling = null; }
async function pollJob(ticket = generation) {
  if (!current || ticket !== generation) return;
  const job = await api(projectApi('/job'));
  if (ticket !== generation) return;
  lastJob = job; setBusy(job?.status === 'running');
  if (job) renderJob(job);
  if (busy) polling = setTimeout(safely(() => pollJob(ticket)), 700);
  else if (job) await refreshCards(ticket);
}
async function refreshCards(ticket) {
  const data = await api(projectApi()); if (ticket !== generation) return;
  cards = data.cards;
  if (!dirty) { selected = cards.find(c => c.id === selected?.id) || cards[0] || null; renderEditor(); }
  renderCards();
}
function renderJob(job) {
  $('job-panel').hidden = false;
  $('job-title').textContent = job.kind === 'export' ? '导出卡片包 / 学习资料' : '生成卡片';
  $('job-message').textContent = job.message;
  $('retry-failed').hidden = job.kind !== 'generate' || job.status === 'running' || !job.failed_cards?.length;
  const seconds = job.status === 'running' && jobStarted ? Math.max(job.elapsed_seconds, Math.floor((Date.now() - jobStarted) / 1000)) : job.elapsed_seconds;
  $('job-time').textContent = `已用时 ${seconds} 秒`;
  const progress = $('job-progress');
  if (job.status === 'running' && (['database', 'prepare', 'finishing'].includes(job.phase) || !job.total)) progress.removeAttribute('value');
  else progress.value = job.status === 'done' ? 100 : (job.phase === 'media' && job.total_bytes ? job.written_bytes / job.total_bytes * 100 : job.total ? job.current / job.total * 100 : 0);
  $('job-errors').hidden = !job.errors.length; $('errors-text').textContent = job.errors.join('\n\n');
  $('export-result').hidden = !job.output_path;
  if (job.output_path) {
    $('output-path').textContent = job.output_path;
    $('download-result').href = '/api' + projectApi('/download');
    $('download-result').download = job.output_path.split(/[\\/]/).pop();
    $('export-result').querySelector('.hint').textContent = job.output_path.endsWith('.apkg') ? '在 Anki 中选择「文件 → 导入」，选取上面的 .apkg 文件。' : 'Markdown 文件已保存，可在本地编辑器中打开。';
  }
}
async function startJob(path, body) {
  if (dirty) { notice('请先保存卡片修改再开始任务。', true); return; }
  setBusy(true); jobStarted = Date.now();
  renderJob({ kind: path === '/export' ? 'export' : 'generate', status: 'running', phase: 'prepare', current: 0, total: 0, media_files: 0, elapsed_seconds: 0, errors: [], message: '正在检查项目并准备任务，请等待…' });
  try { await api(projectApi(path), jsonOptions('POST', body)); stopPolling(); await pollJob(); }
  catch (error) { setBusy(false); $('job-panel').hidden = true; throw error; }
}
async function showSettings() {
  const [config, presets] = await Promise.all([api('/config'), api('/model-presets')]);
  settingsDraft = config; catalog = presets; ensureServiceConnections();
  const form = $('settings-form');
  for (const [key, value] of Object.entries(config)) {
    const element = form.elements.namedItem(key); if (!element) continue;
    if (element.type === 'checkbox') element.checked = Boolean(value);
    else element.value = value;
  }
  renderConnections(); refreshSelections(); clearTestResults(); setModelPurpose('text');
  $('settings-error').hidden = true;
  setSettingsTab(config.connections.some(c => c.id === config.text_connection && c.has_api_key) ? 'generation' : 'connections');
  $('settings-dialog').showModal();
}
let defaultProjectPath = '';
function projectDirectory(name) {
  const cleaned = name.trim().replace(/[\\/:*?"<>|\u0000-\u001f]/g, '_').replace(/[. ]+$/g, '').replace(/^\.+/g, '') || '英语单词';
  return 'projects/' + cleaned;
}
function updateProjectDirectory() {
  const form = $('new-form');
  if (!form.elements.path.value || form.elements.path.value === defaultProjectPath) {
    defaultProjectPath = projectDirectory(form.elements.name.value); form.elements.path.value = defaultProjectPath;
  }
}
function openNew() {
  const form = $('new-form'); if (!form.elements.name.value) form.elements.name.value = '英语单词';
  updateProjectDirectory(); $('new-dialog').showModal();
}
$('new-form').elements.name.addEventListener('input', updateProjectDirectory);
$('settings-button').addEventListener('click', safely(showSettings));
for (const id of ['new-button', 'welcome-new']) $(id).addEventListener('click', openNew);
for (const button of document.querySelectorAll('[data-close]')) button.addEventListener('click', () => $(button.dataset.close).close());
$('search').addEventListener('input', renderCards);
for (const [fileId, formId] of [['words-file', 'new-form'], ['import-file', 'import-form']]) {
  $(fileId).addEventListener('change', safely(async event => {
    const file = event.target.files[0]; if (!file) return;
    if (!file.name.toLowerCase().endsWith('.json')) throw new Error('请选择 JSON 文件；课本单词表可以拍照上传。');
    if (file.size > 1024 * 1024) throw new Error('单词 JSON 请控制在 1 MB 内。');
    const text = await file.text();
    if (!Array.isArray(JSON.parse(text))) throw new Error('JSON 文件应为单词卡片数组。');
    $(formId).elements.words.value = text;
  }));
}
const photoStates = {};
function readPhoto(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader(); reader.onload = () => resolve(reader.result);
    reader.onerror = () => reject(new Error('无法读取照片，请重试。')); reader.readAsDataURL(file);
  });
}
for (const [prefix, formId, dialogId] of [['new', 'new-form', 'new-dialog'], ['import', 'import-form', 'import-dialog']]) {
  const state = photoStates[prefix] = { images: [], urls: [], revision: 0, busy: false };
  const form = $(formId), button = $(prefix + '-extract'), status = $(prefix + '-extract-result');
  $(prefix + '-photos').addEventListener('change', safely(async event => {
    const revision = ++state.revision, files = [...event.target.files]; state.images = []; button.disabled = true;
    for (const url of state.urls) URL.revokeObjectURL(url); state.urls = [];
    $(prefix + '-photo-preview').replaceChildren(); status.textContent = '';
    if (!files.length) return;
    if (files.length > 6 || files.some(file => file.size > 8 * 1024 * 1024) || files.reduce((sum, file) => sum + file.size, 0) > 12 * 1024 * 1024) throw new Error('最多上传 6 张照片，单张不超过 8 MB，总大小不超过 12 MB。');
    if (files.some(file => !['image/jpeg', 'image/png', 'image/webp'].includes(file.type))) throw new Error('请选择 JPG、PNG 或 WebP 照片。');
    const images = await Promise.all(files.map(readPhoto)); if (revision !== state.revision) return;
    state.images = images;
    for (const [index, url] of images.entries()) { const img = node('img'); const preview = URL.createObjectURL(files[index]); state.urls.push(preview); img.src = preview; img.alt = files[index].name; $(prefix + '-photo-preview').append(img); }
    button.disabled = state.busy;
  }));
  button.addEventListener('click', safely(async () => {
    if (!state.images.length || state.busy) return;
    if (form.elements.words.value.trim() && !confirm('识别结果将替换当前单词列表，继续？')) return;
    const revision = state.revision, previousWords = form.elements.words.value; state.busy = true; button.disabled = true;
    const submit = form.querySelector('[type=submit]'); submit.disabled = true;
    status.textContent = '正在识别单词表，请等待…';
    try {
      const result = await api('/extract-words', jsonOptions('POST', { images: state.images }));
      if (revision !== state.revision || !$(dialogId).open) return;
      if (form.elements.words.value !== previousWords) { status.textContent = '单词列表已修改，本次识别结果未覆盖修改，请重新识别。'; return; }
      form.elements.words.value = JSON.stringify(result.words, null, 2);
      status.textContent = `${result.message}，共 ${result.words.length} 个单词。`;
    } catch (error) { if (revision === state.revision) status.textContent = error.message; }
    finally { state.busy = false; button.disabled = !state.images.length; submit.disabled = false; }
  }));
  form.addEventListener('reset', () => {
    state.revision++; state.images = []; for (const url of state.urls) URL.revokeObjectURL(url); state.urls = []; button.disabled = true; status.textContent = ''; $(prefix + '-photo-preview').replaceChildren();
  });
  $(dialogId).addEventListener('close', () => { state.revision++; });
}
$('import-button').addEventListener('click', () => $('import-dialog').showModal());
$('import-form').addEventListener('submit', safely(async event => {
  event.preventDefault();
  if (photoStates.import.busy) return;
  const result = await api(projectApi('/import'), jsonOptions('POST', { words: event.target.elements.words.value }));
  $('import-dialog').close(); event.target.reset(); await refreshCards(generation); notice(result.message);
}));
$('new-form').addEventListener('submit', safely(async event => {
  event.preventDefault(); if (photoStates.new.busy) return; const form = event.target; $('create-submit').disabled = true;
  try {
    const project = await api('/projects', jsonOptions('POST', Object.fromEntries(new FormData(form))));
    $('new-dialog').close(); form.reset(); defaultProjectPath = ''; await loadProjects(); await selectProject(project);
  } finally { $('create-submit').disabled = false; }
}));
function selectedConnection(kind) {
  const id = $('settings-form').elements[purposes[kind].connection].value;
  return settingsDraft.connections.find(connection => connection.id === id);
}
const settingsPanels = { connections: 'connection-settings', generation: 'generation-settings', advanced: 'advanced-settings' };
function activateTab(names, prefix, panels, selected) {
  for (const name of names) {
    const active = name === selected, button = $(prefix + name);
    button.setAttribute('aria-selected', String(active)); button.tabIndex = active ? 0 : -1;
    $(panels[name]).hidden = !active;
  }
}
function setSettingsTab(tab) {
  activateTab(Object.keys(settingsPanels), 'tab-', settingsPanels, tab);
}
function setModelPurpose(kind) {
  activateTab(['text', 'vision', 'image', 'audio'], 'purpose-',
    { text: 'model-text', vision: 'model-vision', image: 'model-image', audio: 'model-audio' }, kind);
}
function bindTabs(names, prefix, select) {
  names.forEach((name, index) => {
    $(prefix + name).addEventListener('click', () => select(name));
    $(prefix + name).addEventListener('keydown', event => {
      if (!['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) return;
      event.preventDefault();
      const next = event.key === 'Home' ? 0 : event.key === 'End' ? names.length - 1
        : (index + (event.key === 'ArrowRight' ? 1 : -1) + names.length) % names.length;
      select(names[next]); $(prefix + names[next]).focus();
    });
  });
}
bindTabs(Object.keys(settingsPanels), 'tab-', setSettingsTab);
bindTabs(['text', 'vision', 'image', 'audio'], 'purpose-', setModelPurpose);
$('manage-connections').addEventListener('click', () => { setSettingsTab('connections'); $('tab-connections').focus(); });
function inputLabel(title, value, update, type = 'text') {
  const label = node('label', title), input = node(type === 'textarea' ? 'textarea' : 'input');
  if (type !== 'textarea') input.type = type; else input.rows = 3;
  input.value = value || ''; input.addEventListener('input', () => update(input.value));
  label.append(input); return { label, input };
}
function connectionDefaults(provider) {
  return { ark: 'https://ark.cn-beijing.volces.com/api/v3',
    doubao: 'https://openspeech.bytedance.com/api/v3/tts/unidirectional',
    }[provider];
}
function ensureServiceConnections() {
  for (const [provider, name] of [['ark', '火山方舟'], ['doubao', '豆包语音']]) {
    if (!settingsDraft.connections.some(connection => connection.provider === provider)) {
      settingsDraft.connections.push({ id: crypto.randomUUID(), name, provider,
        base_url: connectionDefaults(provider), api_key: '', models: [], text_url: '', image_url: '', tts_url: '' });
    }
  }
  for (const [kind, purpose] of Object.entries(purposes)) {
    if (!settingsDraft[purpose.connection]) settingsDraft[purpose.connection] = settingsDraft.connections.find(c => supports(c, kind)).id;
  }
}
function connectionAdvanced(connection, invalidateKey) {
  const advanced = node('details', undefined, 'settings__connection-advanced');
  advanced.append(node('summary', '地址与高级选项'));
  advanced.append(inputLabel('API 基础地址', connection.base_url, value => {
    connection.base_url = value; invalidateKey();
  }, 'url').label, node('p', '更改地址后，需要重新填写 Key。', 'hint'));
  appendConnectionOverrides(advanced, connection, invalidateKey);
  return advanced;
}
function appendConnectionOverrides(container, connection, invalidateKey) {
  const details = node('details'); details.append(node('summary', '自定义模型列表与接口'));
  details.append(node('p', '模型 ID 每行一个。也可直接在「模型用途」中选择自定义 ID。', 'hint'));
  for (const [kind, title, field] of [['text', '文本', 'text_url'], ['vision', '识图', 'text_url'], ['image', '配图', 'image_url'], ['audio', '语音', 'tts_url']]) {
    if (!supports(connection, kind)) continue;
    const models = (connection.models || []).filter(model => model.kind === kind).map(model => model.id).join('\n');
    details.append(inputLabel(title + '模型 ID', models, value => {
      const ids = [...new Set(value.split(/\r?\n/).map(id => id.trim()).filter(Boolean))];
      connection.models = (connection.models || []).filter(model => model.kind !== kind)
        .concat(ids.map(id => ({ id, name: id, kind })));
      refreshSelections(); clearTestResults();
    }, 'textarea').label);
    if (kind !== 'vision') details.append(inputLabel(title + '完整接口地址（可选）', connection[field], value => {
      connection[field] = value; invalidateKey();
    }, 'url').label);
  }
  container.append(details);
}
function connectionCredentials(connection) {
  const label = node('label', 'API Key'), key = node('input'), status = node('p', undefined, 'hint');
  key.type = 'password'; key.autocomplete = 'new-password'; key.value = connection.api_key || ''; label.append(key);
  const clear = node('button', '清除 Key', 'settings__text-button'); clear.type = 'button';
  const update = () => {
    key.placeholder = connection.has_api_key && !connection.clear_key ? '已保存，留空保留' : '粘贴 API Key';
    status.textContent = connection.api_key ? '新 Key 将在保存后生效' : connection.clear_key ? 'Key 已清除，保存后生效' : connection.has_api_key ? 'Key 已保存' : '尚未填写 Key';
    clear.disabled = !connection.api_key && !(connection.has_api_key && !connection.clear_key);
    clearTestResults();
  };
  key.addEventListener('input', () => {
    connection.api_key = key.value; if (key.value) connection.clear_key = false; update();
  });
  const invalidateKey = () => {
    connection.api_key = ''; connection.clear_key = true; key.value = ''; update();
  };
  clear.addEventListener('click', invalidateKey); update();
  const row = node('div', undefined, 'settings__key-status'); row.append(status, clear);
  return { label, row, invalidateKey };
}
function renderConnections() {
  $('connections').replaceChildren();
  for (const connection of settingsDraft.connections) {
    const box = node('div', undefined, 'settings__connection');
    const heading = node('div', undefined, 'settings__section-head');
    const title = node('h3', connection.name || '未命名连接');
    heading.append(title, node('span', connection.provider === 'doubao' ? '语音' : '文本 / 识图 / 配图', 'settings__tag'));
    const credentials = connectionCredentials(connection);
    const link = node('a', '获取 API Key ↗');
    link.href = connection.provider === 'doubao' ? 'https://console.volcengine.com/speech/new/setting/apikeys?projectName=default' : catalog.console_url;
    link.target = '_blank'; link.rel = 'noreferrer'; link.className = 'settings__console';
    const advanced = connectionAdvanced(connection, credentials.invalidateKey);
    advanced.addEventListener('input', () => { title.textContent = connection.name || '未命名连接'; });
    const kind = connection.provider === 'doubao' ? 'audio' : 'text';
    const actions = node('div', undefined, 'test-actions');
    const button = node('button', '验证此连接', 'secondary connection-test');
    button.type = 'button'; button.dataset.modelTest = '';
    const result = node('div', undefined, 'test-result');
    result.hidden = true; result.dataset.modelTestResult = ''; result.setAttribute('aria-live', 'polite');
    actions.append(button, node('span', kind === 'audio' ? '生成示例语音；云服务可能收费' : '生成示例文本；云服务可能收费', 'hint'));
    button.addEventListener('click', safely(async () => {
      const config = settingsData(), purpose = purposes[kind];
      if (config[purpose.connection] !== connection.id || !config[purpose.model].trim()) {
        config[purpose.model] = modelOptions(kind, connection)[0]?.id || '';
      }
      config[purpose.connection] = connection.id;
      if (kind === 'audio') {
        config.audio_backend = 'doubao';
        if (!config.tts_voice.trim()) config.tts_voice = catalog.voices[0]?.id || '';
      }
      await testService(kind, config, result);
    }));
    box.append(heading, credentials.label, credentials.row, link, advanced, actions, result); $('connections').append(box);
  }
}
function modelOptions(kind, connection) {
  const curated = connection?.provider === 'ark' && kind !== 'audio' || connection?.provider === 'doubao' && kind === 'audio';
  return [...(curated ? catalog[purposes[kind].catalog] : []), ...(connection?.models || []).filter(model => model.kind === kind)]
    .filter((model, index, models) => models.findIndex(other => other.id === model.id) === index);
}
function fillModelSelect(id, customId, field, models) {
  const form = $('settings-form'), value = form.elements[field].value, select = $(id); select.replaceChildren();
  for (const model of models) { const option = node('option', model.name || model.id); option.value = model.id; select.append(option); }
  const custom = node('option', '自定义 ID'); custom.value = ''; select.append(custom);
  select.value = models.some(model => model.id === value) ? value : ''; $(customId).hidden = Boolean(select.value);
}
function refreshSelections() {
  const form = $('settings-form');
  for (const [kind, purpose] of Object.entries(purposes)) {
    const select = form.elements[purpose.connection], value = select.value || settingsDraft[purpose.connection];
    select.replaceChildren(); const empty = node('option', '请选择服务连接'); empty.value = ''; select.append(empty);
    for (const connection of settingsDraft.connections.filter(c => supports(c, kind))) {
      const option = node('option', connection.name || '未命名连接'); option.value = connection.id; select.append(option);
    }
    select.value = [...select.options].some(option => option.value === value) ? value : '';
    select.closest('label').hidden = settingsDraft.connections.filter(c => supports(c, kind)).length === 1;
    settingsDraft[purpose.connection] = select.value;
    fillModelSelect(purpose.preset, purpose.custom, purpose.model, modelOptions(kind, selectedConnection(kind)));
  }
  refreshVoice(); syncEnabled();
}
function refreshVoice() {
  const connection = selectedConnection('audio');
  const voices = connection?.provider === 'doubao' ? catalog.voices : [];
  fillModelSelect('voice-preset', 'voice-custom', 'tts_voice', voices);
  $('voice-help').textContent = connection?.provider === 'doubao' ? '音色需与模型匹配；声音复刻需先开通对应音色。' : '填写所用语音服务支持的音色 ID。';
}
function syncEnabled() {
  const form = $('settings-form');
  for (const [kind, enabled] of [['audio', form.elements.audio_enabled.checked], ['image', form.elements.images_enabled.checked]]) {
    $(kind + '-options').hidden = !enabled;
    $(kind + '-disabled').hidden = enabled;
    for (const element of $(kind + '-options').querySelectorAll('input, select, button')) element.disabled = !enabled;
  }
  $('system-audio-option').hidden = !settingsDraft.system_audio_available;
  $('system-audio-option').disabled = !settingsDraft.system_audio_available;
  const system = form.elements.audio_backend.value === 'system';
  $('doubao-audio-options').hidden = system; $('system-audio-options').hidden = !system;
  for (const element of $('doubao-audio-options').querySelectorAll('input, select')) element.disabled = system || !form.elements.audio_enabled.checked;
  form.elements.system_voice.disabled = !system || !form.elements.audio_enabled.checked;
  const thinking = selectedConnection('text')?.provider === 'ark';
  $('thinking-option').hidden = !thinking; form.elements.text_thinking.disabled = !thinking;
  for (const button of document.querySelectorAll('[data-model-test]')) {
    if (testing) button.disabled = true;
  }
  $('save-settings').disabled = testing;
}
for (const [kind, purpose] of Object.entries(purposes)) {
  $('settings-form').elements[purpose.connection].addEventListener('change', () => {
    const form = $('settings-form'), connection = selectedConnection(kind), options = modelOptions(kind, connection);
    settingsDraft[purpose.connection] = form.elements[purpose.connection].value;
    form.elements[purpose.model].value = options[0]?.id || '';
    fillModelSelect(purpose.preset, purpose.custom, purpose.model, options);
    if (kind === 'audio') { form.elements.tts_voice.value = connection?.provider === 'doubao' ? catalog.voices[0]?.id || '' : ''; refreshVoice(); }
    syncEnabled(); clearTestResults();
  });
  $(purpose.preset).addEventListener('change', () => {
    const form = $('settings-form'); form.elements[purpose.model].value = $(purpose.preset).value;
    $(purpose.custom).hidden = Boolean($(purpose.preset).value); clearTestResults();
  });
}
$('voice-preset').addEventListener('change', () => {
  $('settings-form').elements.tts_voice.value = $('voice-preset').value;
  $('voice-custom').hidden = Boolean($('voice-preset').value); clearTestResults();
});
for (const name of ['audio_enabled', 'images_enabled', 'audio_backend']) $('settings-form').elements[name].addEventListener('change', syncEnabled);
function settingsData() {
  const form = $('settings-form'), data = { connections: settingsDraft.connections.map(({ has_api_key, clear_key, ...connection }) => ({ ...connection })) };
  for (const key of ['text_connection', 'vision_connection', 'image_connection', 'tts_connection', 'openai_model', 'vision_model', 'image_model', 'tts_model', 'tts_voice', 'audio_backend', 'system_voice']) data[key] = form.elements[key].value;
  for (const key of ['audio_enabled', 'images_enabled', 'text_thinking']) data[key] = form.elements[key].checked;
  data.port = Number(form.elements.port.value); data.concurrency = Number(form.elements.concurrency.value);
  data.clear_connections = settingsDraft.connections.filter(c => c.clear_key).map(c => c.id);
  return data;
}
function validateSettings(data, onlyKind) {
  if (!Number.isInteger(data.port) || data.port < 0 || data.port > 65535 ||
      !Number.isInteger(data.concurrency) || data.concurrency < 1 || data.concurrency > 16) { setSettingsTab('advanced'); throw new Error('请填写有效的端口（0–65535）和任务数（1–16）。'); }
  for (const connection of data.connections) {
    if (!connection.name.trim()) { setSettingsTab('connections'); throw new Error('请填写服务连接名称。'); }
    for (const url of [connection.base_url, connection.text_url, connection.image_url, connection.tts_url].filter(Boolean)) {
      try { const parsed = new URL(url); if (!['http:', 'https:'].includes(parsed.protocol) || parsed.username || parsed.password) throw new Error(); }
      catch (_) { setSettingsTab('connections'); throw new Error('请为「' + connection.name + '」填写完整的 HTTP/HTTPS 服务地址。'); }
    }
    if (!connection.base_url) { setSettingsTab('connections'); throw new Error('请填写服务连接地址。'); }
  }
  const kinds = onlyKind ? [onlyKind] : ['text', ...(data.images_enabled ? ['image'] : []), ...(data.audio_enabled ? ['audio'] : [])];
  if (!onlyKind && data.vision_connection) kinds.push('vision');
  for (const kind of kinds) {
    const purpose = purposes[kind];
    if (kind === 'audio' && data.audio_backend === 'system') continue;
    if (kind === 'audio') {
      const connection = settingsDraft.connections.find(c => c.id === data.tts_connection);
      if (!connection?.api_key && !(connection?.has_api_key && !connection.clear_key)) {
        setSettingsTab('connections'); throw new Error('使用豆包语音时，请先填写语音连接的 API Key。');
      }
    }
    if (!data[purpose.connection] || !data[purpose.model].trim() || kind === 'audio' && !data.tts_voice.trim()) {
      setSettingsTab('generation'); setModelPurpose(kind);
      throw new Error('请完成「' + ({ text: '文本', vision: '识图', image: '配图', audio: '语音' })[kind] + '」的服务连接、模型' + (kind === 'audio' ? '和音色' : '') + '设置。');
    }
  }
}
function clearTestResults() {
  testRevision++;
  for (const result of document.querySelectorAll('[data-model-test-result]')) { result.hidden = true; result.replaceChildren(); result.classList.remove('success', 'error'); delete result.dataset.status; }
}
let testing = false;
function serviceTestConfig(kind, config) {
  // Test only the selected service, even when other settings are unfinished.
  const field = purposes[kind].connection;
  const id = kind === 'audio' && config.audio_backend === 'system' ? '' : config[field];
  return { ...config, connections: config.connections.filter(c => c.id === id),
    ...Object.fromEntries(Object.values(purposes).map(p => [p.connection, p.connection === field ? id : ''])),
    clear_connections: config.clear_connections.filter(value => value === id),
    port: 0, concurrency: 1, audio_enabled: kind === 'audio', images_enabled: kind === 'image',
    audio_backend: kind === 'audio' ? config.audio_backend : 'doubao' };
}
async function testService(kind, config, result = $('test-' + kind + '-result')) {
  if (testing) return;
  $('settings-error').hidden = true;
  config = serviceTestConfig(kind, config);
  try { validateSettings(config, kind); }
  catch (error) { showSettingsError(error); return 'invalid'; }
  testing = true; const revision = testRevision;
  result.hidden = false; result.dataset.status = 'testing'; result.classList.remove('error', 'success'); result.replaceChildren(node('span', '正在调用模型，请等待…'));
  for (const button of document.querySelectorAll('[data-model-test]')) button.disabled = true;
  syncEnabled();
  try {
    const data = await api('/test', jsonOptions('POST', { kind, config }));
    if (revision !== testRevision || !$('settings-dialog').open) return 'stale';
    result.classList.add('success'); result.dataset.status = 'success';
    result.replaceChildren(node('div', `连接成功 · ${data.connection || '当前服务'} · ${data.model} · ${(data.duration_ms / 1000).toFixed(1)} 秒`));
    result.append(node('div', data.message));
    if (data.words) result.append(node('div', data.words.map(word => [word.name, word.symbol, word.chn].filter(Boolean).join(' / ')).join('\n')));
    if (data.card) result.append(node('div', `${data.card.name} / ${data.card.symbol || ''}\n${data.card.chn}\n${data.card.example_en}\n${data.card.example_cn || ''}\n${data.card.memory_tips || ''}`));
    if (data.asset_url) {
      const media = node(kind === 'audio' ? 'audio' : 'img'); media.src = data.asset_url;
      if (kind === 'audio') { media.controls = true; media.preload = 'metadata'; } else media.alt = '图片模型测试结果';
      result.append(media);
    }
    return 'success';
  } catch (error) {
    if (revision === testRevision && $('settings-dialog').open) { result.classList.add('error'); result.dataset.status = 'error'; result.replaceChildren(node('div', '验证失败：' + error.message)); return 'error'; }
    return 'stale';
  } finally {
    testing = false; for (const button of document.querySelectorAll('[data-model-test]')) button.disabled = false;
    syncEnabled();
  }
}
for (const kind of ['text', 'vision', 'audio', 'image']) $('test-' + kind).addEventListener('click', safely(async () => {
  await testService(kind, settingsData());
}));
function showSettingsError(error) {
  $('settings-error').textContent = error.message; $('settings-error').hidden = false;
  const panel = document.querySelector('.settings__body > section:not([hidden])');
  if (panel.id === 'connection-settings') {
    for (const details of panel.querySelectorAll('details')) details.open = true;
  }
}
$('settings-form').addEventListener('input', () => { clearTestResults(); $('settings-error').hidden = true; });
$('settings-dialog').addEventListener('close', clearTestResults);
$('settings-form').addEventListener('submit', async event => {
  event.preventDefault(); $('settings-error').hidden = true;
  try {
    const data = settingsData(); validateSettings(data); $('save-settings').disabled = true;
    const result = await api('/config', jsonOptions('PUT', data));
    mediaSettings = data; renderCards(); $('settings-dialog').close(); notice(result.message);
  } catch (error) {
    showSettingsError(error);
  } finally { $('save-settings').disabled = false; }
});
$('card-form').addEventListener('input', () => { dirty = true; $('dirty-note').textContent = '有未保存的修改'; });
$('card-form').addEventListener('submit', safely(async event => {
  event.preventDefault(); if (!selected) return;
  const data = { ...selected.data, ...Object.fromEntries(new FormData(event.target)) };
  const result = await api(projectApi('/cards/' + encodeURIComponent(selected.id)), jsonOptions('PUT', data));
  dirty = false; await refreshCards(generation); notice(result.message);
}));
$('regenerate-one').addEventListener('click', safely(() => {
  if (!selected) return;
  const scope = $('regenerate-scope').value;
  if (confirm('重新生成会替换此卡所选内容，并调用付费服务。继续？')) return startJob('/generate', { card: selected.id, scope, force: true });
}));
$('retry-failed').addEventListener('click', safely(() => startJob('/generate', { retry_failed: true })));
$('generate-all').addEventListener('click', safely(() => startJob('/generate', {})));
$('generate-one').addEventListener('click', safely(() => startJob('/generate', { card: selected.id })));
$('export-apkg').addEventListener('click', safely(() => startJob('/export', { format: 'apkg' })));
$('export-md').addEventListener('click', safely(() => startJob('/export', { format: 'md' })));
for (const id of ['open-project', 'open-result']) $(id).addEventListener('click', safely(async () => { const result = await api(projectApi('/open'), { method: 'POST' }); notice(result.message); }));
$('copy-path').addEventListener('click', safely(async () => {
  await navigator.clipboard.writeText(lastJob.output_path); notice('本地文件路径已复制');
}));
window.addEventListener('beforeunload', event => { if (dirty) { event.preventDefault(); event.returnValue = ''; } });
(async () => {
  try { await loadProjects(); if (projects.length) await selectProject(projects[0]); }
  catch (error) { notice(error.message, true); }
})();
