const tauri = window.__TAURI__;
const previewSettings={
  wake_words:['ривер','ревер','ривьер'],
  match_threshold:0.7,
  asr_threads:4,languages:['en','ru'],models_unload_s:60,local_index:true,
  vad_threshold:0.5,wake_pause_ms:600,command_pause_ms:900,command_wait_ms:5000,
  partial_results:true,cancel_words:['отмена','стоп','отбой','забудь'],
  raise_result:true,activation_mode:'both',hotkey:'Ctrl+Shift+G',confirm_dangerous:true,confirm_close:false,
  sound_pack:'default',private_log:false,offline_mode:false,mute_hotkey:'',
  dictation_clipboard:true,dictation_live:true,dictation_english:true,dictation_english_level:3,dictation_idle_ms:3000,dictation_pause_ms:800,webchat_send:true,wake_spotter:true,wake_sound:true,mascot_mode:'always',mascot_scale:100,mascot_bubble:true
};
const defaultGrammarPreview=()=>({
  wake_words:['ривер','ревер','ривьер','ливер'],
  cancel_words:['отмена','стоп','отбой','забудь','хватит','остановись','stop','cancel'],
  dictation_stop:['стоп ввод','ривер остановись','stop typing'],
  intents:[
    {id:'launch',kind:'launch',label:'открыть программу или ссылку',triggers:['','открой','запусти','open','launch','run'],url:'',enabled:true,lang:'auto'},
    {id:'folder',kind:'folder',label:'открыть папку на компьютере',triggers:['папка','открой папку','folder','open folder'],url:'',enabled:true,lang:'auto'},
    {id:'web_google',kind:'search',label:'поиск в Google',triggers:['загугли','google search','search google'],url:'https://www.google.com/search?q={q}',enabled:true,lang:'auto'},
    {id:'web_youtube',kind:'search',label:'поиск на YouTube',triggers:['поиск ютуб','search youtube','youtube search'],url:'https://www.youtube.com/results?search_query={q}',enabled:true,lang:'auto'},
    {id:'close',kind:'close',label:'закрыть открытую программу',triggers:['закрой','close','quit'],url:'',enabled:true,lang:'auto'},
    {id:'kill',kind:'kill',label:'завершить программу (как в диспетчере задач)',triggers:['заверши','kill','end task'],url:'',enabled:true,lang:'auto'},
    {id:'calc',kind:'calc',label:'калькулятор',triggers:['посчитай','сколько будет','calculate','how much is'],url:'',enabled:true,lang:'auto'},
    {id:'dictate',kind:'dictate',label:'диктовка: ввести текст в поле с курсором',triggers:['напиши','запиши','диктовка','type','write','dictate'],url:'',enabled:true,lang:'auto'},
    {id:'find',kind:'find',label:'найти файл или папку (окно выбора)',triggers:['найди','найти','поищи','find'],url:'',enabled:true,lang:'auto'},
    {id:'find_folder',kind:'find_folder',label:'найти папку (окно выбора)',triggers:['найди папку','найти папку','поищи папку','find folder'],url:'',enabled:true,lang:'auto'},
    {id:'find_file',kind:'find_file',label:'найти файл (окно выбора)',triggers:['найди файл','найти файл','поищи файл','find file'],url:'',enabled:true,lang:'auto'},
    {id:'chat_gpt',kind:'web_chat',label:'спросить ChatGPT (чат в браузере)',triggers:['чат гпт','чатгпт','чат джипити','чат джи пити','чат жпт','chatgpt','chat gpt'],url:'https://chatgpt.com/',enabled:true,lang:'auto'},
    {id:'chat_claude',kind:'web_chat',label:'спросить Claude (чат в браузере)',triggers:['клауд','клод','клауде','клоуд','claude'],url:'https://claude.ai/new',enabled:true,lang:'auto'},
    {id:'chat_gemini',kind:'web_chat',label:'спросить Gemini (чат в браузере)',triggers:['гемини','гимини','гимине','джемини','джимини','gemini'],url:'https://gemini.google.com/app',enabled:true,lang:'auto'}
  ]
});
let previewGrammar=defaultGrammarPreview();
// Подписи намерений по умолчанию (как в grammar.rs): если пользователь их не менял,
// показываем перевод, а не русскую строку из grammar.json.
const DEFAULT_INTENT_LABELS={
  launch:'открыть программу или ссылку',folder:'открыть папку на компьютере',
  web_google:'поиск в Google',web_youtube:'поиск на YouTube',close:'закрыть открытую программу',
  kill:'завершить программу (как в диспетчере задач)',calc:'посчитать в калькуляторе',
  dictate:'диктовка: ввести текст в поле с курсором',find:'найти файл или папку (окно выбора)',
  find_folder:'найти папку (окно выбора)',find_file:'найти файл (окно выбора)',
  chat_gpt:'спросить ChatGPT (чат в браузере)',chat_claude:'спросить Claude (чат в браузере)',
  chat_gemini:'спросить Gemini (чат в браузере)'
};
function isDefaultLabel(intent){
  const label=String(intent?.label||'').trim();
  const builtIn=DEFAULT_INTENT_LABELS[intent?.id];
  // Старые подписи по умолчанию из прежних версий grammar.json — тоже «не менял».
  return !!builtIn&&(!label||label===builtIn||(intent.id==='calc'&&label==='калькулятор')
    ||(intent.id==='folder'&&label==='открыть папку на компьютере (Everything)'));
}
function intentLabel(intent){
  if(isDefaultLabel(intent)) return t('intent.'+intent.id);
  return String(intent?.label||'').trim()||intent?.id||'';
}
// Та же проверка, что в Rust (Grammar::validate), но на языке интерфейса — до сохранения.
function grammarErrors(g){
  const errors=[];
  if(!g?.wake_words?.some(w=>String(w).trim())) errors.push(t('validation.wake'));
  const ids=new Set(), phrases=new Map();
  for(const intent of g?.intents||[]){
    const name=intentLabel(intent);
    if(!String(intent.id||'').trim()) errors.push(t('validation.id'));
    else if(ids.has(intent.id)) errors.push(t('validation.duplicateId',{id:intent.id}));
    ids.add(intent.id);
    if(intent.kind==='search'&&!String(intent.url||'').includes('{q}')) errors.push(t('validation.searchQ',{name}));
    if(intent.kind==='web_chat'&&!/^https?:\/\//.test(String(intent.url||'').trim())) errors.push(t('validation.chatUrl',{name}));
    if((intent.kind==='calc'||intent.kind==='dictate')&&!(intent.triggers||[]).some(phrase=>String(phrase).trim())) errors.push(t('validation.needPhrase',{name}));
    for(const phrase of intent.triggers||[]){
      const key=String(phrase).trim().toLocaleLowerCase();
      if(!key&&intent.kind!=='launch') errors.push(t('validation.emptyPhrase',{name}));
      if(key&&phrases.has(key)) errors.push(t('validation.duplicatePhrase',{phrase,other:phrases.get(key)}));
      else if(key) phrases.set(key,name);
    }
  }
  return errors;
}
const cloneValue=value=>JSON.parse(JSON.stringify(value));
const invoke = tauri?.core?.invoke || (async(command,commandArgs={})=>{
  switch(command){
    case 'get_settings': return {...previewSettings};
    case 'get_grammar': return cloneValue(previewGrammar);
    case 'save_grammar': {
      const errors=grammarErrors(commandArgs.grammar);
      if(errors.length) throw new Error(errors.join('\n'));
      previewGrammar=cloneValue(commandArgs.grammar); return undefined;
    }
    case 'reset_grammar': previewGrammar=defaultGrammarPreview(); return cloneValue(previewGrammar);
    case 'list_actions': return [];
    case 'list_apps': return [];
    case 'list_sound_packs': return ['default'];
    case 'language_models': return {languages:{ru:'european',de:'european',fr:'european',es:'european',it:'european',pt:'european',zh:'asian',ja:'asian'},installed:['european']};
    case 'get_paused': case 'get_autostart': return false;
    case 'index_status': return {state:'ready',entries:0,bytes:0};
    case 'get_wallpaper': return null;
    default: return undefined;
  }
});
const listen = tauri?.event?.listen || (async()=>()=>{});
const $ = (s)=>document.querySelector(s);
const el=(t,p={},k=[])=>{const e=document.createElement(t);Object.assign(e,p);for(const c of k)e.append(c);return e;};

let appWindow=null;
try{ appWindow = tauri?.window?.getCurrentWindow?.() || null; }catch(e){ reportError('window',e); }
// context — ключ error.<context> из словаря.
function reportError(context,error,params={}){
  const message=t('error.prefix',{context:t('error.'+context,params),message:error?.message||error});
  console.error(message,error);
  appendLog(message);
}
function subscribe(event,handler){
  try{ Promise.resolve(listen(event,handler)).catch(e=>reportError('subscribe',e,{event})); }
  catch(e){ reportError('subscribe',e,{event}); }
}
$('#winMin').onclick=()=>{ try{ appWindow?.minimize(); }catch(e){ reportError('minimize',e); } };
$('#winClose').onclick=()=>{ try{ appWindow?.hide(); }catch(e){ reportError('hide',e); } };

// Один геометрический монохромный набор: смешивать эмодзи и глифы —
// самый заметный признак несобранного интерфейса.
// Иконки — один нарисованный набор (символы <symbol id="i-…"> в index.html),
// одна толщина линии; юникод-глифы вместо иконок выглядят собранными из чего попало.
const svgNS='http://www.w3.org/2000/svg';
function icon(name,cls='ico'){
  const svg=document.createElementNS(svgNS,'svg'); svg.setAttribute('class',cls); svg.setAttribute('aria-hidden','true');
  const use=document.createElementNS(svgNS,'use'); use.setAttribute('href','#i-'+name); svg.append(use); return svg;
}
const KIND_ICON={shell:'play',url:'link',uwp:'grid',folder:'folder',system:'power',search:'lens'};
const KIND_GROUP={shell:'apps',uwp:'apps',url:'web',search:'web',folder:'folder',system:'system'};

// Навигация. Пункты — не <button>, чтобы не бороться с корпусом кнопки
// Aero, но клавиатуру обслуживают полноценно.
function selectNav(n){
  if(n.classList.contains('on')) return;
  document.querySelectorAll('.navi').forEach(x=>{ x.classList.remove('on'); x.setAttribute('aria-selected','false'); });
  n.classList.add('on'); n.setAttribute('aria-selected','true');
  // Экран переключается как канал ЭЛТ: старое схлопывается в строку, новое раскрывается.
  const swap=()=>{ document.querySelectorAll('.panel').forEach(x=>x.classList.remove('on')); $('#p-'+n.dataset.nav).classList.add('on'); };
  if(window.Skin?.channel) window.Skin.channel(swap); else swap();
}
document.querySelectorAll('.navi').forEach(n=>{
  n.onclick=()=>selectNav(n);
  n.onkeydown=(e)=>{ if(e.key==='Enter'||e.key===' '){ e.preventDefault(); selectNav(n); } };
});

let paused=false, micState='idle';
const pill=(key,cls='')=>el('span',{className:'pill'+(cls?' '+cls:''),textContent:t(key)});
function setDot(p){
  micState=p;
  const d=$('#dot'), l=$('#micLabel');
  d.className='dot'+(p==='listening'?' listening':p==='busy'?' busy':p==='loading'?' loading':p==='error'?' error':'');
  l.dataset.i18n=['listening','busy','loading','paused','error'].includes(p)?'mic.'+p:'mic.idle';
  l.textContent=t(l.dataset.i18n);
  if(p==='idle'||p==='paused') refreshPause();
}
function renderPause(){ $('#pauseBtn').textContent=t(paused?'mic.resume':'mic.pause'); $('#t-mic').replaceChildren(paused?pill('pill.paused','bad'):pill('pill.active','ok')); }
async function refreshPause(){ try{ paused=await invoke('get_paused'); renderPause(); }catch(e){ reportError('checkMic',e); } }
$('#pauseBtn').onclick=async()=>{ try{ await invoke('set_paused',{paused:!paused}); await refreshPause(); }catch(e){ reportError('toggleMic',e); } };

subscribe('reever://state', e=>setDot(String(e.payload)));
subscribe('reever://partial', e=>{ const v=String(e.payload||''); $('#partial').textContent=v||'…'; $('#partial2').textContent=v; });
subscribe('reever://heard', e=>{ const v=String(e.payload||'').trim(); if(v){ const last=$('#lastCmd'); last.dataset.heard='1'; last.textContent=v; } });
subscribe('reever://status', e=>applyStatus(e.payload));
subscribe('reever://log', e=>appendLog(String(e.payload)));
let calId=null;
function renderCalCount(){ if(!calId) return; const a=actions.find(x=>x.id===calId); const ph=a&&a.phrases?a.phrases:[]; $('#calHeading').textContent=t('calibration.heading',{label:a?.label||calId}); $('#calCount').textContent=t('calibration.count',{count:ph.length}); $('#calLast').textContent=ph.length?t('calibration.last',{phrase:ph[ph.length-1]}):''; }
subscribe('reever://calibrated', async ()=>{ await loadActions(); renderCalCount(); });

function appendLog(t){ const d=el('div',{className:'l',textContent:t}); const L=$('#log'); L.append(d); while(L.childNodes.length>250)L.removeChild(L.firstChild); L.scrollTop=L.scrollHeight; }
let lastStatus=null;
function applyStatus(payload){
  let status=payload;
  if(typeof status==='string'){ try{status=JSON.parse(status);}catch(e){reportError('status',e);return;} }
  if(!status||typeof status!=='object') return;
  lastStatus=status;
  const asr=status.asr;
  $('#t-asr').replaceChildren(asr==='ready'?pill('pill.ready','ok'):asr==='error'?pill('pill.error','bad'):asr==='loading'?pill('pill.loading'):el('span',{className:'pill',textContent:'—'}));
  $('#t-model').replaceChildren(el('span',{textContent:status.model||'—'}));
}

let actions=[], selId=null, filter='all', query='', editorDirty=false;
async function loadActions(){ actions=await invoke('list_actions'); editorDirty=false; $('#t-count').textContent=actions.length; renderRows(); renderEditor(); }
async function confirmDiscardActionEdits(){
  if(!editorDirty) return true;
  if(!confirm(t('actions.confirmDiscard'))) return false;
  const previousId=selId;
  await loadActions();
  selId=previousId;
  return true;
}
$('#search').oninput=()=>{ query=$('#search').value.toLowerCase(); renderRows(); };
document.querySelectorAll('#filters .chip').forEach(c=>{
  const pick=()=>{ document.querySelectorAll('#filters .chip').forEach(x=>x.classList.remove('on')); c.classList.add('on'); filter=c.dataset.f; renderRows(); };
  c.onclick=pick;
  c.onkeydown=(e)=>{ if(e.key==='Enter'||e.key===' '){ e.preventDefault(); pick(); } };
});

function visible(a){
  if(filter!=='all' && KIND_GROUP[a.kind||'shell']!==filter) return false;
  if(query && !((a.label||'').toLowerCase().includes(query) || (a.phrases||[]).some(p=>p.includes(query)))) return false;
  return true;
}
function renderRows(){
  const R=$('#rows'); R.innerHTML='';
  const list=actions.filter(visible);
  if(!list.length){ R.append(el('div',{className:'hint',style:'padding:12px;',textContent:t('actions.empty')})); return; }
  for(const a of list){
    const row=el('div',{className:'arow'+(a.id===selId?' sel':'')});
    row.append(el('div',{className:'ai'},[icon(KIND_ICON[a.kind||'shell']||'play')]));
    row.append(el('div',{className:'an',textContent:a.label||t('actions.unnamed')}));
    row.append(el('div',{className:'ac',textContent:t('actions.phraseCount',{count:(a.phrases||[]).length})}));
    row.onclick=async()=>{
      if(a.id===selId) return;
      if(!await confirmDiscardActionEdits()) return;
      selId=a.id; renderRows(); renderEditor();
    };
    R.append(row);
  }
}
const KINDS=['shell','url','uwp','folder','system','search'];
function renderEditor(){
  const E=$('#editor'); const a=actions.find(x=>x.id===selId);
  if(!a){ E.innerHTML=`<div class="empty">${t('actions.pick')}</div>`; return; }
  E.innerHTML='';
  const head=el('div',{className:'editrow'});
  const label=el('input',{value:a.label||'',placeholder:t('actions.namePlaceholder')}); label.className='grow'; label.oninput=()=>{a.label=label.value; editorDirty=true;};
  const kind=el('select'); kind.style.width='auto'; for(const k of KINDS){const o=el('option',{value:k,textContent:t('actionKind.'+k)});if((a.kind||'shell')===k)o.selected=true;kind.append(o);} kind.onchange=()=>{a.kind=kind.value; editorDirty=true;};
  head.append(label,kind); E.append(head);

  E.append(el('label',{className:'fld',textContent:t('actions.target')}));
  const target=el('input',{value:a.target||'',placeholder:'steam://rungameid/570'}); target.oninput=()=>{a.target=target.value; editorDirty=true;}; E.append(target);
  E.append(el('div',{className:'hint',style:'margin-top:3px;',textContent:t('actions.searchHint')}));
  const pick=el('button',{className:'sm',textContent:t('actions.pickApp')}); pick.style.marginTop='6px'; pick.onclick=()=>openAppPicker(a,target); E.append(pick);

  E.append(el('label',{className:'fld',textContent:t('actions.phrases',{count:(a.phrases||[]).length})}));
  const chips=el('div',{className:'phrases'});
  for(const p of (a.phrases||[])){ const x=el('b',{title:t('actions.removePhrase')},[icon('x')]); x.onclick=async()=>{if(!await confirmDiscardActionEdits())return;await invoke('remove_phrase',{id:a.id,phrase:p});await loadActions();}; chips.append(el('span',{className:'pchip'},[document.createTextNode(p),x])); }
  E.append(chips);
  const addRow=el('div',{className:'editrow',style:'margin-top:7px;'});
  const pInp=el('input',{placeholder:t('actions.phrasePlaceholder')}); pInp.className='grow';
  const doAdd=async()=>{const v=pInp.value.trim();if(!v)return; await invoke('save_action',{action:a}); editorDirty=false; await invoke('add_phrase',{id:a.id,phrase:v}); pInp.value=''; await loadActions();};
  const pAdd=el('button',{className:'sm',textContent:t('common.add')}); pAdd.onclick=doAdd; pInp.onkeydown=(e)=>{if(e.key==='Enter'){e.preventDefault();doAdd();}};
  addRow.append(pInp,pAdd); E.append(addRow);

  const foot=el('div',{className:'editrow',style:'margin-top:16px;'});
  // Калибровка и сохранение — одного вида, удаление — отдельное, пунктиром.
  const cal=el('button',{className:'default',textContent:t('actions.calibrate')}); cal.onclick=()=>calibrate(a);
  const save=el('button',{className:'default',textContent:t('common.save')}); save.onclick=async()=>{ if(!(a.label||'').trim()){alert(t('actions.needName'));return;} await invoke('save_action',{action:a}); editorDirty=false; await loadActions(); };
  const del=el('button',{className:'danger',textContent:t('common.delete')}); del.onclick=async()=>{ if(!await confirmDiscardActionEdits())return; if(confirm(t('actions.confirmDelete'))){await invoke('delete_action',{id:a.id}); selId=null; await loadActions();} };
  foot.append(cal,save,del); E.append(foot);
}
$('#addBtn').onclick=async()=>{ if(!await confirmDiscardActionEdits())return; const id='act_'+Date.now().toString(36); actions.push({id,label:'',kind:'shell',target:'',verbs:[],phrases:[]}); selId=id; editorDirty=true; renderRows(); renderEditor(); };

async function calibrate(a){ await invoke('save_action',{action:a}); editorDirty=false; await invoke('start_calibration',{id:a.id}); calId=a.id; renderCalCount(); $('#calOverlay').classList.add('on'); }
$('#calDone').onclick=async()=>{ try{await invoke('cancel_calibration');}catch(e){ reportError('cancelCalibration',e); } calId=null; $('#calOverlay').classList.remove('on'); };

let appsCache=null;
async function openAppPicker(a,targetInput){
  const list=$('#appList'); $('#appPicker').classList.add('on'); $('#appSearch').value='';
  list.replaceChildren(el('div',{className:'hint',textContent:t('common.loading')}));
  if(!appsCache){ try{appsCache=await invoke('list_apps');}catch(e){appsCache=[];reportError('loadApps',e);} }
  const render=(f)=>{ list.innerHTML=''; const q=(f||'').toLowerCase();
    for(const ap of appsCache){ if(q&&!ap.name.toLowerCase().includes(q))continue;
      const it=el('div',{className:'appitem',textContent:ap.name});
      it.onclick=()=>{ a.kind='uwp'; a.target=ap.aumid; if(!a.label)a.label=ap.name; editorDirty=true; targetInput.value=ap.aumid; $('#appPicker').classList.remove('on'); renderEditor(); renderRows(); };
      list.append(it); }
    if(!list.childNodes.length) list.append(el('div',{className:'hint',textContent:t('common.nothingFound')}));
  };
  $('#appSearch').oninput=()=>render($('#appSearch').value); render('');
}
$('#appCancel').onclick=()=>$('#appPicker').classList.remove('on');
// Esc закрывает любое открытое модальное окно.
window.addEventListener('keydown',(e)=>{
  if(e.key!=='Escape') return;
  if($('#appPicker').classList.contains('on')) $('#appPicker').classList.remove('on');
  else if($('#calOverlay').classList.contains('on')) $('#calDone').click();
});

let grammar=defaultGrammarPreview(), grammarDraft=null;
const grammarEditor=$('#grammarEditor');
// Языки распознавания — как RECOGNITION_LANGUAGES в config.rs: английский включён
// всегда, остальным нужен скачанный языковой пакет (models.rs). Новый язык — строка
// здесь и там.
const RECOGNITION_LANGS=['en','ru','de','fr','es','it','pt','zh','ja'];
function recognitionLanguages(settings=settingsObj){
  const chosen=(Array.isArray(settings?.languages)?settings.languages:['en']).map(value=>String(value).trim().toLowerCase());
  return RECOGNITION_LANGS.filter(code=>code==='en'||chosen.includes(code));
}
// Фразы на главной — на языке интерфейса: русские, если интерфейс русский и русский
// распознаётся; иначе английские (английский включён всегда).
const isCyrillic=value=>/[а-яё]/i.test(value);
function phraseLanguage(){ return I18N.locale==='ru'&&recognitionLanguages().includes('ru')?'ru':'en'; }
function pickPhrase(values,fallback=''){
  const list=(values||[]).map(value=>String(value).trim()).filter(Boolean);
  const russian=phraseLanguage()==='ru';
  return list.find(value=>isCyrillic(value)===russian)||list[0]||fallback;
}
function grammarWake(){ return (grammar?.wake_words||[]).find(word=>String(word).trim())||'ривер'; }
// Wake-слово в английских примерах: «ривер» английская модель слышит как «river».
function displayWake(){
  const words=(grammar?.wake_words||[]).map(word=>String(word).trim()).filter(Boolean);
  if(phraseLanguage()==='ru') return words[0]||'ривер';
  const latin=words.find(word=>!isCyrillic(word));
  if(latin) return latin;
  return !words.length||words[0].toLowerCase()==='ривер'?'river':words[0];
}
function renderHomeHint(){
  const last=$('#lastCmd');
  if(last.dataset.heard) return;
  const launch=(grammar?.intents||[]).find(intent=>intent.kind==='launch'&&intent.enabled);
  last.textContent=t('home.hint',{wake:displayWake(),open:pickPhrase(launch?.triggers,phraseLanguage()==='ru'?'открой':'open')});
}
function grammarExampleRow(command,description){
  const row=el('div',{className:'row'});
  row.append(el('code',{textContent:command}),el('span',{textContent:description}));
  return row;
}
function renderGrammarExamples(){
  const host=$('#grammarExamples'); host.replaceChildren();
  const wake=displayWake(), russian=phraseLanguage()==='ru';
  renderHomeHint();
  for(const intent of grammar.intents||[]){
    if(!intent.enabled) continue;
    const triggers=(intent.triggers||[]).map(value=>String(value).trim());
    const bare=intent.kind==='launch'&&triggers.includes('');
    const trigger=pickPhrase(triggers);
    if(bare){
      host.append(grammarExampleRow(`${wake} X`,t('example.launch',{label:intentLabel(intent)})));
      continue;
    }
    if((intent.kind==='close'||intent.kind==='kill')&&trigger){
      const help=t(intent.kind==='close'?'example.close':'example.kill');
      host.append(grammarExampleRow(wake+' '+trigger+' X',help));
      continue;
    }
    if(intent.kind==='calc'&&trigger){
      host.append(grammarExampleRow(`${wake} ${trigger} X`,t('example.calc',{expr:russian?'33 умножить на 78':'33 times 78'})));
      continue;
    }
    if((intent.kind==='find'||intent.kind==='find_folder'||intent.kind==='find_file')&&trigger){
      const help=intent.kind==='find_folder'?'example.findFolders':intent.kind==='find_file'?'example.findFiles':'example.findBoth';
      host.append(grammarExampleRow(`${wake} ${trigger} X`,t(help)));
      continue;
    }
    if(intent.kind==='web_chat'&&trigger){
      host.append(grammarExampleRow(`${wake} ${trigger} X`,t('example.chat',{label:intentLabel(intent)||t('example.chatDefault')})));
      continue;
    }
    if(intent.kind==='dictate'&&trigger){
      const stop=pickPhrase(grammar.dictation_stop,russian?'стоп ввод':'stop typing');
      // «{wake} стоп» заканчивает диктовку, «{wake} отмена» стирает (engine.rs: is_dictation_stop).
      const end=russian?'стоп':'stop';
      const cancel=pickPhrase((grammar.cancel_words||[]).filter(word=>!['стоп','stop','хватит','остановись'].includes(String(word).trim().toLowerCase())),russian?'отмена':'cancel');
      host.append(grammarExampleRow(`${wake} ${trigger} …`,t('example.dictate',{wake,stop,cancel,end})));
      continue;
    }
    if(trigger) host.append(grammarExampleRow(`${wake} ${trigger} X`,intentLabel(intent)||t('example.command')));
  }
  host.append(grammarExampleRow(`${wake} ${russian?'выключи комп':'shut down computer'}`,t('example.shutdown')));
  host.append(grammarExampleRow(`${wake} ${pickPhrase(grammar.cancel_words,russian?'отмена':'cancel')}`,t('example.cancel')));
}
async function loadGrammar(){
  $('#grammarPreviewNote').hidden=!!tauri;
  try{
    const value=await invoke('get_grammar');
    if(value&&Array.isArray(value.wake_words)&&Array.isArray(value.cancel_words)&&Array.isArray(value.intents)) grammar=value;
    else throw new Error(t('grammar.badFormat'));
  }catch(error){
    reportError('loadGrammar',error);
    grammar=defaultGrammarPreview();
  }
  renderGrammarExamples();
}
// kind: wake | phrase | cancel | stop — подписи из словаря grammar.add*/keepOne*.
function addChipEditor(parent,getValues,setValues,kind,minimum=0){
  const box=el('div',{className:'grammar-chipbox'});
  const chips=el('div',{className:'grammar-chips'});
  const clearHint=()=>{const hint=parent.querySelector('.grammar-small-note');if(hint)hint.textContent='';};
  const refresh=()=>{
    chips.replaceChildren();
    getValues().forEach((value,index)=>{
      const item=el('span',{className:'grammar-chip'});
      item.append(el('span',{textContent:value}),el('button',{type:'button',className:'sm grammar-chip-remove',title:t('grammar.removeChip',{value}),ariaLabel:t('grammar.removeChip',{value})},[icon('x')]));
      if(kind==='wake'&&index===0) item.classList.add('primary');
      item.lastChild.onclick=()=>{
        const next=getValues().filter((_,i)=>i!==index);
        if(next.length<minimum){ const hint=parent.querySelector('.grammar-small-note');if(hint)hint.textContent=t(kind==='wake'?'grammar.keepOneWake':'grammar.keepOnePhrase'); return; }
        setValues(next); refresh();
      };
      chips.append(item);
    });
  };
  const line=el('div',{className:'editrow grammar-chip-add'});
  const addLabel=t({wake:'grammar.addWake',phrase:'grammar.addPhrase',cancel:'grammar.addCancel',stop:'grammar.addStop'}[kind]);
  const input=el('input',{placeholder:addLabel,ariaLabel:addLabel});
  const add=()=>{const value=input.value.trim();if(!value)return;setValues([...getValues(),value]);input.value='';clearHint();refresh();};
  input.onkeydown=event=>{if(event.key==='Enter'){event.preventDefault();add();}};
  line.append(input,el('button',{type:'button',className:'sm',textContent:t('common.add'),onclick:add}));
  box.append(chips,line); parent.append(box); refresh();
}
function renderGrammarEditor(){
  grammarEditor.replaceChildren();
  const wakeGroup=el('fieldset',{className:'grp'}); wakeGroup.append(el('legend',{textContent:t('grammar.wakeLegend')}));
  wakeGroup.append(el('div',{className:'hint grammar-small-note',textContent:t('grammar.wakeHint')}));
  addChipEditor(wakeGroup,()=>grammarDraft.wake_words||[],values=>grammarDraft.wake_words=values,'wake',1);
  grammarEditor.append(wakeGroup);
  for(const intent of grammarDraft.intents||[]){
    const group=el('fieldset',{className:'grp grammar-intent'});
    const kindIcons={launch:'play',folder:'folder',search:'lens',close:'x',kill:'stop',calc:'sum',dictate:'pen',web_chat:'spark',find:'lens',find_folder:'folder',find_file:'file'};
    const legend=el('legend');
    const kindIcon=el('span',{className:'grammar-kind-icon'},[icon(kindIcons[intent.kind]||'spark')]);
    kindIcon.setAttribute('aria-hidden','true');
    legend.append(kindIcon,document.createTextNode(' '+(I18N.has('kind.'+intent.kind)?t('kind.'+intent.kind):t('grammar.intent'))));
    group.append(legend);
    const head=el('div',{className:'grammar-intent-head'});
    const enabledWrap=el('label',{className:'cbrow grammar-enabled'});
    const enabled=el('input',{type:'checkbox'}); enabled.checked=!!intent.enabled; enabled.onchange=()=>intent.enabled=enabled.checked;
    enabledWrap.append(enabled,el('span',{textContent:t('grammar.enabled')}));
    const labelWrap=el('label',{className:'grammar-label-field'}); labelWrap.append(el('span',{className:'fld',textContent:t('grammar.label')}));
    const labelInput=el('input',{value:isDefaultLabel(intent)?intentLabel(intent):(intent.label||'')}); labelInput.oninput=()=>intent.label=labelInput.value; labelWrap.append(labelInput);
    head.append(enabledWrap,labelWrap);
    if((intent.kind==='search'&&String(intent.id).startsWith('search_'))||(intent.kind==='web_chat'&&String(intent.id).startsWith('chat_custom_'))){
      const remove=el('button',{type:'button',className:'sm danger grammar-delete-intent',textContent:t(intent.kind==='web_chat'?'grammar.deleteChat':'grammar.deleteSearch')});
      remove.onclick=()=>{grammarDraft.intents=grammarDraft.intents.filter(item=>item!==intent);renderGrammarEditor();}; head.append(remove);
    }
    group.append(head);
    const phraseLabel=el('div',{className:'fld grammar-field-label',textContent:t('grammar.phrases')}); group.append(phraseLabel);
    const getTriggers=()=>intent.triggers||[];
    const setTriggers=values=>intent.triggers=values;
    const visibleTriggers=()=>getTriggers().filter(value=>String(value).trim());
    const triggerHost=el('div'); group.append(triggerHost);
    const minimumTriggers=(intent.kind==='close'||intent.kind==='kill'||intent.kind==='calc'||intent.kind==='dictate'||intent.kind==='web_chat'||intent.kind==='find'||intent.kind==='find_folder'||intent.kind==='find_file')?1:0;
    if(minimumTriggers) triggerHost.append(el('div',{className:'hint grammar-small-note'}));
    addChipEditor(triggerHost,visibleTriggers,values=>{
      const hasBare=intent.kind==='launch'&&getTriggers().some(value=>!String(value).trim());
      intent.triggers=hasBare?['',...values]:values;
    },'phrase',minimumTriggers);
    if(intent.kind==='launch'){
      const bareRow=el('label',{className:'cbrow grammar-bare-launch'});
      const bare=el('input',{type:'checkbox'}); bare.checked=getTriggers().some(value=>!String(value).trim());
      bare.onchange=()=>{
        const rest=getTriggers().filter(value=>String(value).trim());
        intent.triggers=bare.checked?['',...rest]:rest;
      };
      bareRow.append(bare,el('span',{textContent:t('grammar.bareLaunch',{wake:displayWake(),app:phraseLanguage()==='ru'?'фотошоп':'photoshop'})}));
      group.append(bareRow);
    }
    if(intent.kind==='web_chat'){
      const urlWrap=el('label',{className:'grammar-url-field'}); urlWrap.append(el('span',{className:'fld',textContent:t('grammar.chatUrl')}));
      const urlInput=el('input',{value:intent.url||'',placeholder:'https://gemini.google.com/app'}); urlInput.oninput=()=>intent.url=urlInput.value; urlWrap.append(urlInput); group.append(urlWrap);
    }
    if(intent.kind==='search'){
      const urlWrap=el('label',{className:'grammar-url-field'}); urlWrap.append(el('span',{className:'fld',textContent:t('grammar.searchUrl')}));
      const urlInput=el('input',{value:intent.url||'',placeholder:'https://example.com/?q={q}'}); urlInput.oninput=()=>intent.url=urlInput.value; urlWrap.append(urlInput); group.append(urlWrap);
    }
    grammarEditor.append(group);
  }
  const addSearch=el('button',{type:'button',className:'sm grammar-add-search',textContent:t('grammar.addSearch')});
  addSearch.onclick=()=>{
    let timestamp=Date.now();
    while(grammarDraft.intents.some(intent=>intent.id===`search_${timestamp}`)) timestamp++;
    const id=`search_${timestamp}`;
    grammarDraft.intents.push({id,kind:'search',label:t('grammar.newSearch'),triggers:['поиск'],url:'https://example.com/?q={q}',enabled:true,lang:'auto'});
    renderGrammarEditor();
  };
  grammarEditor.append(addSearch);
  const addChat=el('button',{type:'button',className:'sm grammar-add-search',textContent:t('grammar.addChat')});
  addChat.onclick=()=>{
    let timestamp=Date.now();
    while(grammarDraft.intents.some(intent=>intent.id===`chat_custom_${timestamp}`)) timestamp++;
    grammarDraft.intents.push({id:`chat_custom_${timestamp}`,kind:'web_chat',label:t('grammar.newChat'),triggers:['чат'],url:'https://chatgpt.com/',enabled:true,lang:'auto'});
    renderGrammarEditor();
  };
  grammarEditor.append(addChat);
  const cancelGroup=el('fieldset',{className:'grp'}); cancelGroup.append(el('legend',{textContent:t('grammar.cancelLegend')}));
  cancelGroup.append(el('div',{className:'hint grammar-small-note',textContent:t('grammar.cancelHint')}));
  addChipEditor(cancelGroup,()=>grammarDraft.cancel_words||[],values=>grammarDraft.cancel_words=values,'cancel');
  grammarEditor.append(cancelGroup);
  const stopGroup=el('fieldset',{className:'grp'}); stopGroup.append(el('legend',{textContent:t('grammar.stopLegend')}));
  stopGroup.append(el('div',{className:'hint grammar-small-note',textContent:t('grammar.stopHint')}));
  addChipEditor(stopGroup,()=>grammarDraft.dictation_stop||[],values=>grammarDraft.dictation_stop=values,'stop',1);
  grammarEditor.append(stopGroup);
  const actions=el('div',{className:'grammar-actions'});
  const save=el('button',{type:'button',className:'default',textContent:t('common.save')});
  const cancel=el('button',{type:'button',className:'sm',textContent:t('common.cancel')});
  const reset=el('button',{type:'button',className:'sm danger',textContent:t('grammar.reset')});
  const errors=el('ul',{className:'grammar-errors',hidden:true,ariaLive:'polite'});
  const setErrors=error=>{
    errors.replaceChildren();
    const details=String(error?.message||error||t('grammar.saveFailed')).split(/\r?\n/).filter(Boolean);
    for(const message of details) errors.append(el('li',{textContent:message}));
    errors.hidden=!details.length;
  };
  save.onclick=async()=>{
    save.disabled=true; errors.hidden=true;
    try{
      const problems=grammarErrors(grammarDraft);
      if(problems.length) throw new Error(problems.join('\n'));
      await invoke('save_grammar',{grammar:grammarDraft});
      grammar=cloneValue(grammarDraft); grammarDraft=null; grammarEditor.hidden=true; setGrammarButton();
      renderGrammarExamples();
    }catch(error){setErrors(error);reportError('saveGrammar',error);}
    finally{save.disabled=false;}
  };
  cancel.onclick=()=>{grammarDraft=null;grammarEditor.hidden=true;setGrammarButton();};
  reset.onclick=async()=>{
    if(!window.confirm(t('grammar.confirmReset')))return;
    reset.disabled=true;errors.hidden=true;
    try{
      const result=await invoke('reset_grammar');
      grammar=result&&Array.isArray(result.intents)?result:defaultGrammarPreview();
      grammarDraft=null;grammarEditor.hidden=true;setGrammarButton();renderGrammarExamples();
    }catch(error){setErrors(error);reportError('resetGrammar',error);}
    finally{reset.disabled=false;}
  };
  actions.append(save,cancel,reset); grammarEditor.append(actions,errors);
}
function setGrammarButton(){
  const button=$('#editGrammarBtn');
  button.dataset.i18n=grammarDraft?'grammar.closeEditor':'grammar.edit';
  button.textContent=t(button.dataset.i18n);
}
$('#editGrammarBtn').onclick=()=>{
  if(grammarDraft){grammarDraft=null;grammarEditor.hidden=true;setGrammarButton();return;}
  grammarDraft=cloneValue(grammar);renderGrammarEditor();grammarEditor.hidden=false;setGrammarButton();
};
$('#grammarSettingsLink').onclick=event=>{
  event.preventDefault();selectNav(document.querySelector('[data-nav="home"]'));
  if(!grammarDraft)$('#editGrammarBtn').click();
  $('#grammarPanel').scrollIntoView({block:'start',behavior:'smooth'});
};

let settingsObj={};
let indexStatus=null,indexStatusNode=null,indexStatusTimer=null;
// [ключ настройки, тип, варианты, когда применяется]; подпись — settings.<ключ>,
// подсказка — settings.<ключ>.help, если она есть в словаре.
const GROUPS=[
  ['interface',[['ui_language','ui_language',null,'instant']]],
  ['mascot',[['mascot_mode','mascot_mode',['always','commands','never'],'instant'],['mascot_scale','mascot_scale',null,'instant'],['mascot_bubble','bool',null,'instant']]],
  ['activation',[['activation_mode','select',['both','wake','hotkey']],['wake_spotter','bool'],['wake_sound','bool'],['hotkey','hotkey',null,'instant'],['mute_hotkey','hotkey',null,'instant']]],
  ['privacy',[['private_log','bool'],['offline_mode','bool']]],
  ['languages',[['languages','languages']]],
  ['recognition',[
    ['asr_threads','number'],['models_unload_s','number'],['vad_threshold','number'],['wake_pause_ms','number'],
    ['command_pause_ms','number'],['command_wait_ms','number'],['partial_results','bool'],['match_threshold','number']]],
  ['dictation',[
    ['dictation_clipboard','bool'],['dictation_live','bool'],['dictation_english','bool'],['dictation_english_level','strength'],
    ['dictation_idle_ms','number'],['dictation_pause_ms','number'],['webchat_send','bool']]],
  ['search',[['local_index','bool']]],
  ['system',[['confirm_dangerous','bool'],['confirm_close','bool'],['raise_result','bool']]],
];
// Англицизмы — только для русской диктовки.
const SETTING_NEEDS={dictation_english:'ru',dictation_english_level:'ru'};
function settingVisible(key){
  const need=SETTING_NEEDS[key];
  return !need||recognitionLanguages().includes(need);
}
// Всё применяется по «Сохранить»; пометка «— сразу» — у того, что действует уже при
// изменении поля (язык интерфейса, хоткеи), без «Сохранить».
const settingLabel=(key,when='save')=>when==='instant'?`${t('settings.'+key)} — ${t('settings.instant')}`:t('settings.'+key);
const settingHelp=key=>I18N.has(`settings.${key}.help`)?t(`settings.${key}.help`):null;
let cbSeq=0;
// Индекс дисков: плитка на Главной и строка под переключателем в настройках.
function renderIndexStatus(){
  const state=indexStatus?.state;
  const known=['off','scanning','ready'].includes(state);
  const tile=$('#t-index');
  if(tile) tile.replaceChildren(!known?pill('pill.unchecked'):state==='ready'?pill('pill.indexReady','ok')
    :state==='scanning'?pill('pill.indexing'):pill('pill.off'));
  $('#ledNet')?.classList.toggle('on', state==='ready');
  if(!indexStatusNode) return;
  if(!known){
    indexStatusNode.hidden=true;
    return;
  }
  const entries=new Intl.NumberFormat().format(Number(indexStatus.entries)||0);
  const mib=(Number(indexStatus.bytes)||0)/(1024*1024);
  const size=`${new Intl.NumberFormat(undefined,{maximumFractionDigits:1}).format(mib)} MiB`;
  indexStatusNode.textContent=t('settings.indexStatus',{state:t(`settings.indexState.${state}`),entries,size});
  indexStatusNode.hidden=false;
}
async function refreshIndexStatus(){
  try{
    const status=await invoke('index_status');
    indexStatus=status&&['off','scanning','ready'].includes(status.state)?status:null;
  }catch{
    indexStatus=null;
  }
  renderIndexStatus();
}
function startIndexStatusPolling(){
  if(indexStatusTimer) clearInterval(indexStatusTimer);
  refreshIndexStatus();
  indexStatusTimer=setInterval(()=>{ refreshIndexStatus(); },5000);
}
function fieldEl(key,type,opts,when='save'){
  const lbl=settingLabel(key,when), help=settingHelp(key);
  // Чекбокс в семёрке — подпись справа на одной строке, а не сверху.
  if(type==='bool'){
    const wrap=el('div',{className:help?'setting-with-help':''});
    const row=el('div',{className:'cbrow'});
    const id='cb'+(++cbSeq);
    const inp=el('input',{type:'checkbox',id}); inp.checked=key==='mascot_bubble'?settingsObj[key]!==false:!!settingsObj[key];
    // Пузырь маскота включается сразу, без «Сохранить».
    inp.onchange=()=>{ settingsObj[key]=inp.checked; if(key==='mascot_bubble') applyMascot(); };
    const lab=el('label',{htmlFor:id,textContent:lbl});
    row.append(inp,lab); wrap.append(row);
    if(help) wrap.append(el('div',{className:'hint setting-help',textContent:help}));
    return wrap;
  }
  const wrap=el('div'); wrap.append(el('label',{className:'fld',textContent:lbl}));
  let inp;
  if(type==='list'){ inp=el('input',{value:(settingsObj[key]||[]).join(', ')}); inp.oninput=()=>settingsObj[key]=inp.value.split(',').map(s=>s.trim()).filter(Boolean); }
  else if(type==='languages'){
    wrap.append(languagesEditor(languageModels));
    if(help) wrap.append(el('div',{className:'hint setting-help',textContent:help}));
    return wrap;
  }
  else if(type==='ui_language'){
    // Сразу: язык меняется без перезапуска и сохраняется отдельно от кнопки «Сохранить».
    inp=el('select');
    const current=settingsObj[key]||'auto';
    inp.append(el('option',{value:'auto',textContent:`${t('settings.ui_language.auto')} (${I18N.NAMES[I18N.resolve('auto')]})`}));
    for(const code of I18N.SUPPORTED){
      inp.append(el('option',{value:code,textContent:I18N.NAMES[code]}));
    }
    inp.value=I18N.SUPPORTED.includes(current)?current:'auto';
    inp.onchange=()=>changeLanguage(inp.value);
  }
  else if(type==='mascot_mode'){
    // Сразу: маскот появляется, прячется или исчезает без «Сохранить».
    inp=el('select');
    for(const o of opts){ const op=el('option',{value:o,textContent:t(`settings.${key}.${o}`)}); if((settingsObj[key]||opts[0])===o) op.selected=true; inp.append(op); }
    inp.onchange=()=>{ settingsObj[key]=inp.value; applyMascot(); };
  }
  else if(type==='mascot_scale'){
    // Ползунок 100–500 %: подпись меняется на ходу, окно — когда ползунок отпущен.
    const row=el('div',{className:'mascot-scale'});
    const value=Math.min(500,Math.max(100,Number(settingsObj[key])||100));
    inp=el('input',{type:'range',min:'100',max:'500',step:'25',value:String(value)});
    const out=el('span',{className:'mascot-scale-value',textContent:value+'%'});
    inp.oninput=()=>{ out.textContent=inp.value+'%'; };
    inp.onchange=()=>{ settingsObj[key]=Number(inp.value); applyMascot(); };
    row.append(inp,out); wrap.append(row);
    if(help) wrap.append(el('div',{className:'hint setting-help',textContent:help}));
    return wrap;
  }
  else if(type==='strength'){
    // Ступени 1–5: под ползунком — что делает выбранная ступень, меняется на ходу.
    const row=el('div',{className:'mascot-scale'});
    const value=Math.min(5,Math.max(1,Math.round(Number(settingsObj[key])||3)));
    inp=el('input',{type:'range',min:'1',max:'5',step:'1',value:String(value)});
    const out=el('span',{className:'mascot-scale-value',textContent:`${value} / 5`});
    const about=el('div',{className:'hint setting-help',textContent:t(`settings.${key}.${value}`)});
    inp.oninput=()=>{
      settingsObj[key]=Number(inp.value);
      out.textContent=`${inp.value} / 5`;
      about.textContent=t(`settings.${key}.${inp.value}`);
    };
    row.append(inp,out); wrap.append(row,about);
    return wrap;
  }
  else if(type==='select'){ inp=el('select'); for(const o of opts){const optionKey=`settings.${key}.${o}`;const label=I18N.has(optionKey)?t(optionKey):I18N.has(`mode.${o}`)?t(`mode.${o}`):o;const op=el('option',{value:o,textContent:label});if((settingsObj[key]??opts[0])===o)op.selected=true;inp.append(op);} inp.onchange=()=>settingsObj[key]=inp.value; }
  else if(type==='hotkey'){
    inp=el('button',{type:'button',className:'sm'}); inp.style.minWidth='150px';
    const render=()=>inp.textContent=settingsObj[key]||t('hotkey.none');
    render();
    let rec=false, onKey=null;
    const stop=()=>{ rec=false; if(onKey) window.removeEventListener('keydown',onKey,true); onKey=null; inp.classList.remove('green'); };
    inp.onclick=()=>{
      if(rec){ stop(); render(); return; }
      rec=true; inp.classList.add('green'); inp.textContent=t('hotkey.press');
      onKey=async(ev)=>{
        ev.preventDefault(); ev.stopPropagation();
        if(ev.key==='Escape'){ stop(); render(); return; }
        if(['Control','Alt','Shift','Meta'].includes(ev.key)) return;
        const mods=[]; if(ev.ctrlKey)mods.push('Ctrl'); if(ev.altKey)mods.push('Alt'); if(ev.shiftKey)mods.push('Shift'); if(ev.metaKey)mods.push('Super');
        const c=ev.code; let main=null;
        if(/^Key[A-Z]$/.test(c)) main=c.slice(3);
        else if(/^Digit[0-9]$/.test(c)) main=c.slice(5);
        else if(/^F\d{1,2}$/.test(c)) main=c;
        else if(c==='Space') main='Space';
        else if(c==='Enter') main='Enter';
        else if(c==='Tab') main='Tab';
        else if(c.startsWith('Arrow')) main=c.slice(5);
        if(!main) return;
        if(mods.length===0 && !/^F\d/.test(main)){ inp.textContent=t('hotkey.needModifier'); return; }
        const combo=[...mods,main].join('+');
        settingsObj[key]=combo; stop(); render();
        const cmd = key==='mute_hotkey' ? 'set_mute_hotkey' : 'set_hotkey';
        try{ await invoke(cmd,{hotkey:combo,activationMode:settingsObj.activation_mode||'both'}); $('#settingsMsg').textContent=t('hotkey.applied',{combo}); }
        catch(e){ $('#settingsMsg').textContent=t('hotkey.error',{error:e}); reportError('applyHotkey',e); }
      };
      window.addEventListener('keydown',onKey,true);
    };
  }
  else { inp=el('input',{type,value:settingsObj[key]??''}); if(['vad_threshold','match_threshold'].includes(key)){ inp.step='0.01'; inp.min='0'; inp.max='1'; } if(key==='models_unload_s'){ inp.step='1'; inp.min='0'; } inp.oninput=()=>settingsObj[key]= type==='number'?Number(inp.value):inp.value; }
  wrap.append(inp);
  if(help) wrap.append(el('div',{className:'hint setting-help',textContent:help}));
  if(key==='local_index'){
    indexStatusNode=el('div',{className:'hint setting-help setting-index-status',hidden:true});
    wrap.append(indexStatusNode); renderIndexStatus();
  }
  return wrap;
}
// Языки распознавания: строка на язык с его пакетом; английский не убрать, остальные —
// × и «+ Добавить язык». Язык без скачанного пакета добавить нельзя: пакет ставит
// установщик (src-tauri/windows/models.nsh).
const PACK_NAMES={european:'Parakeet v3',asian:'SenseVoice'};
function languagesEditor(models){
  const box=el('div',{className:'lang-editor'});
  const enabled=recognitionLanguages();
  const name=code=>I18N.NAMES[code]||code;
  const packOf=code=>models?.languages?.[code];
  const installed=pack=>(models?.installed||[]).includes(pack);
  const setLanguages=codes=>{
    settingsObj.languages=codes; renderSettings(); renderGrammarExamples();
    if(grammarDraft) renderGrammarEditor();
    $('#settingsMsg').textContent=t('settings.languages.changed');
  };
  for(const code of enabled){
    const row=el('div',{className:'lang-row'});
    row.append(el('span',{className:'lang-name',textContent:name(code)}));
    if(code==='en'){
      row.append(el('span',{className:'lang-lock',textContent:t('settings.languages.always'),title:t('settings.languages.alwaysTitle')}));
    }else{
      const pack=packOf(code);
      row.append(installed(pack)
        ?el('span',{className:'lang-pack',textContent:PACK_NAMES[pack]||pack})
        :el('span',{className:'lang-pack lang-missing',textContent:t('settings.languages.notDownloaded')}));
      const label=t('settings.languages.remove',{language:name(code)});
      const remove=el('button',{type:'button',className:'sm lang-remove',title:label,ariaLabel:label},[icon('x')]);
      remove.onclick=()=>setLanguages(enabled.filter(item=>item!==code));
      row.append(remove);
    }
    box.append(row);
  }
  const addable=RECOGNITION_LANGS.filter(code=>!enabled.includes(code));
  const addRow=el('div',{className:'lang-add'});
  const add=el('button',{type:'button',className:'sm',textContent:'+ '+t('settings.languages.add')});
  add.setAttribute('aria-haspopup','menu');
  const menu=el('div',{className:'lang-menu',hidden:true,role:'menu'});
  for(const code of addable){
    const ready=installed(packOf(code));
    const item=el('button',{type:'button',className:'sm',role:'menuitem',
      textContent:ready?name(code):`${name(code)} — ${t('settings.languages.notDownloaded')}`});
    item.disabled=!ready;
    item.onclick=()=>setLanguages([...enabled,code]);
    menu.append(item);
  }
  if(addable.length) add.onclick=()=>{ menu.hidden=!menu.hidden; };
  else add.disabled=true;
  addRow.append(add,menu);
  if(!addable.length) addRow.append(el('span',{className:'hint',textContent:t('settings.languages.noMore')}));
  box.append(addRow);
  // Языки из двух пакетов сразу — без определителя языка работает только первый.
  const packs=new Set(enabled.map(packOf).filter(pack=>pack&&installed(pack)));
  if(packs.size>1&&!installed('lid')) box.append(el('div',{className:'hint lang-missing',textContent:t('settings.languages.needLid')}));
  if(addable.some(code=>!installed(packOf(code)))) box.append(el('div',{className:'hint',textContent:t('settings.languages.installer')}));
  return box;
}
let languageModels=null, soundPacks=['default'], autostart=false;
async function loadSettings(){
  try{ settingsObj=await invoke('get_settings'); }
  catch(e){ reportError('loadSettings',e); settingsObj={...previewSettings}; }
  try{ languageModels=await invoke('language_models'); }
  catch(e){ reportError('loadModels',e); }
  try{ autostart=await invoke('get_autostart'); }catch(e){ reportError('readAutostart',e); }
  try{ soundPacks=await invoke('list_sound_packs'); }catch(e){ reportError('loadSoundPacks',e); }
  renderSettings(); renderGrammarExamples(); startIndexStatusPolling();
}
function renderSettings(){
  const F=$('#settingsForm'); F.innerHTML='';
  // Каждая группа — настоящий группбокс с легендой, как в панели
  // управления семёрки.
  for(const [title,fields] of GROUPS){
    const g=el('fieldset',{className:'grp'}); g.append(el('legend',{textContent:t('settings.group.'+title)}));
    for(const f of fields){
      const [key,type,opts,when]=f;
      if(!settingVisible(key)) continue;
      g.append(fieldEl(key,type,opts,when));
    }
    F.append(g);
  }
  const g=el('fieldset',{className:'grp'}); g.append(el('legend',{textContent:t('settings.group.os')}));
  const asRow=el('div',{className:'cbrow'});
  const asCb=el('input',{type:'checkbox',id:'cbAuto'}); asCb.checked=autostart;
  asCb.onchange=async()=>{ try{await invoke('set_autostart',{on:asCb.checked}); autostart=asCb.checked; $('#settingsMsg').textContent=t('settings.autostartState',{state:t(asCb.checked?'common.on':'common.off')});}catch(e){$('#settingsMsg').textContent=t('settings.error',{error:e});reportError('toggleAutostart',e);} };
  asRow.append(asCb, el('label',{htmlFor:'cbAuto',textContent:settingLabel('autostart','instant')}));
  g.append(asRow);
  const spWrap=el('div'); spWrap.append(el('label',{className:'fld',textContent:settingLabel('sound_pack')}));
  const sp=el('select'); for(const p of soundPacks){const o=el('option',{value:p,textContent:p});if((settingsObj.sound_pack??'default')===p)o.selected=true;sp.append(o);}
  sp.onchange=()=>settingsObj.sound_pack=sp.value; spWrap.append(sp); g.append(spWrap);
  F.append(g);
  const mode=settingsObj.activation_mode||'both';
  $('#t-mode').textContent=I18N.has(`mode.${mode}`)?t(`mode.${mode}`):mode;
  $('#t-hotkey').textContent=(settingsObj.activation_mode==='wake')?'—':(settingsObj.hotkey||'—');
}
// Сохранение применяется сразу: Reever перечитывает настройки и перезапускает
// распознавание (несколько секунд загрузки моделей), окно и всё остальное работают.
$('#saveSettingsBtn').onclick=async()=>{ try{await invoke('save_settings',{settings:settingsObj}); $('#settingsMsg').textContent=t('settings.saved');}catch(e){$('#settingsMsg').textContent=t('settings.error',{error:e});reportError('saveSettings',e);} };

// Язык интерфейса: перевести разметку и перерисовать всё, что построено из кода.
// Rust сохраняет выбор в settings.json и переводит меню трея.
function rerender(){
  setDot(micState); renderPause(); if(lastStatus) applyStatus(lastStatus); renderIndexStatus();
  renderRows(); renderEditor(); renderGrammarExamples();
  if(grammarDraft) renderGrammarEditor();
  setGrammarButton(); renderCalCount(); renderSettings();
  $('#settingsMsg').textContent='';
}
async function applyMascot(){
  try{ await invoke('set_mascot',{mode:settingsObj.mascot_mode||'always',scale:Number(settingsObj.mascot_scale)||100,bubble:settingsObj.mascot_bubble!==false}); }
  catch(e){ $('#settingsMsg').textContent=t('settings.error',{error:e}); reportError('mascot',e); }
}
async function changeLanguage(preference){
  settingsObj.ui_language=preference;
  try{ await invoke('set_ui_language',{language:preference}); }catch(e){ reportError('uiLanguage',e); }
  await I18N.setLanguage(preference);
  rerender();
  // Журнал хранится в Rust непереведённым — перерисовываем его на новом языке.
  try{
    const log=(await invoke('get_ui_snapshot'))?.log;
    if(Array.isArray(log)){ const L=$('#log'); L.replaceChildren(...log.map(line=>el('div',{className:'l',textContent:String(line)}))); L.scrollTop=L.scrollHeight; }
  }catch(e){ reportError('uiLanguage',e); }
}

// Сначала язык (из settings.json, иначе язык Windows), потом всё остальное.
(async()=>{
  let preference='auto';
  try{ preference=(await invoke('get_settings'))?.ui_language||'auto'; }catch{}
  await I18N.setLanguage(preference);
  startIndexStatusPolling();
  loadActions().catch(e=>reportError('loadActions',e)); loadSettings(); loadGrammar(); refreshPause();
})();
