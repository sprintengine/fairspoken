// Run against a local Vite server; native IPC is replaced with in-memory fixtures.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE_PATH || 'playwright');
const assert = require('node:assert/strict');
let browser;
(async () => {
 browser = await chromium.launch({ executablePath:process.env.CHROMIUM_EXECUTABLE_PATH,headless:true });
 const page = await browser.newPage({viewport:{width:1000,height:720}});
 await page.addInitScript(() => {
  const callbacks = new Map(); const listeners = new Map(); let next = 1;
  window.smokeSettings = {model:'parakeet-tdt-0.6b-v3',transcriptionLocation:'local',polishEnabled:false,polishProvider:'cloud',polishModel:'qwen3.5-0.8b',cloudAuthToken:'',polishTones:{},vocabularyHints:[],transcriptCorrections:[],snippets:[]};
  window.smokeInstalled = false;
  window.smokeEmit = (event,payload) => (listeners.get(event)||[]).forEach(id=>callbacks.get(id)?.({event,id,payload}));
  window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {unregisterListener:()=>{}};
  window.__TAURI_INTERNALS__ = {
   transformCallback: cb => {const id=next++;callbacks.set(id,cb);return id;},unregisterCallback:()=>{},metadata:{currentWindow:{label:'home'},currentWebview:{label:'home'}},
   invoke: async (cmd,args={}) => {
    if(cmd==='plugin:event|listen'){listeners.set(args.event,[...(listeners.get(args.event)||[]),args.handler]);return next++;}
    if(cmd==='plugin:event|unlisten')return;
    if(cmd==='get_settings')return {...window.smokeSettings};
    if(cmd==='save_settings'){window.smokeSettings=args.settings;window.smokeEmit('settings-updated',args.settings);return;}
    if(cmd==='get_local_model_catalog')return {polish:[{id:'qwen3.5-0.8b',name:'Qwen3.5 0.8B',publisher:'Qwen',description:'Smallest download · Q4 quantization · roughly 1–2 GB memory',bytes:563036064,installed:window.smokeInstalled,selected:window.smokeSettings.polishEnabled && window.smokeSettings.polishProvider==='local',loaded:false,source:'https://huggingface.co/ggml-org/Qwen3.5-0.8B-GGUF',downloads:args.refresh?1000:null,supported:true},{id:'qwen3.5-2b',name:'Qwen3.5 2B',publisher:'Qwen',description:'Larger cleanup model · Q4 quantization · roughly 2–4 GB memory',bytes:1270808032,installed:false,selected:false,loaded:false,source:'https://huggingface.co/lmstudio-community/Qwen3.5-2B-GGUF',downloads:null,supported:true}],download:null,metadataError:null};
    if(cmd==='get_dictation_models')return ['parakeet-tdt-0.6b-v3','parakeet-tdt-0.6b-v2','tiny','base','small','medium','large-v2','large-v3','large-v3-turbo'].map(model=>({model,cached:model==='parakeet-tdt-0.6b-v3'||model==='tiny'}));
    if(cmd==='search_hugging_face_models'){
     if(args.query==='fail')throw 'offline';
     await new Promise(r=>setTimeout(r,args.query==='slow'?900:20));
     const ids={whisper:'ggerganov/whisper.cpp',official:'openai/whisper-tiny',parakeet:'nvidia/parakeet-tdt-0.6b-v2',slow:'openai/whisper-base',fast:'openai/whisper-tiny',unsupported:'org/unsupported'};
     const id=ids[args.query];
     return {models:id?[{id,author:id.split('/')[0],name:id.split('/')[1],downloads:42,likes:1,languages:['en'],gated:false,license:'mit'}]:[],cached:false};
    }
    if(cmd==='download_local_model'){
     window.smokeEmit('local-model-download',{model:args.model,stage:'model',downloaded:280000000,total:563036064,message:'Downloading polish model'});
     await new Promise(r=>setTimeout(r,500));window.smokeInstalled=true;
     window.smokeEmit('local-model-download',{model:args.model,stage:'complete',downloaded:0,total:0,message:'Downloaded'});return;
    }
    if(cmd==='remove_local_model'){window.smokeInstalled=false;window.smokeSettings.polishEnabled=false;return;}
    if(cmd==='get_note_debug_status')return {available:!window.smokeProduction,enabled:false};
    if(cmd==='set_note_debug_capture'){window.smokeCapture=args.enabled;window.smokeEmit('note-debug-status',{available:true,enabled:args.enabled});return;}
    if(cmd==='get_notes')return [{id:'note-1',text:'Original note',createdAt:Date.now(),updatedAt:Date.now(),pinned:false,durationSeconds:5},{id:'note-2',text:'Cleaned note',createdAt:Date.now(),updatedAt:Date.now(),pinned:false,durationSeconds:5}];
    if(cmd==='get_transcript_history')return [];

    if(cmd==='get_transcription_model_status')return {model:'parakeet-tdt-0.6b-v3',cached:true,message:'Ready',modelPath:'/tmp/model'};
    if(cmd==='get_app_status')return {state:'idle',message:'Ready'};
    return {};
   }
  };
 });

 const variantValue = family => page.locator(`#variant-${family}`).getAttribute('data-value');
 const chooseVariant = async (family, value) => {
  await page.locator(`#variant-${family}`).click();
  await page.locator(`#variant-list-${family} [role="option"]`).filter({ hasText: new RegExp(`^${value}\\b`) }).click();
 };

 await page.goto((process.env.FAIRSPOKEN_UI_URL || process.env.MULTIVOICE_UI_URL || 'http://localhost:1420') + '/home.html');
 await page.locator('[data-screen="models"]').click();
 await page.locator('#variant-whisper').waitFor();
 if(await page.locator('#family-parakeet').count()!==1 || await page.locator('#family-whisper').count()!==1)throw Error('families');
 if(await page.locator('#searchModelsButton').count())throw Error('search button');
 if(!await page.locator('#modelSearchBox .model-search-hint').count())throw Error('search glyph');
 await chooseVariant('whisper', 'small');
 assert.equal(await variantValue('whisper'), 'small');
 await page.evaluate(()=>window.smokeEmit('settings-updated',window.smokeSettings));
 await page.waitForTimeout(100);
 assert.equal(await variantValue('whisper'), 'small');
 if(!await page.locator('#variant-whisper').evaluate(e=>e===document.activeElement))throw Error('focus lost');
 await page.locator('#modelSearch').fill('slow'); await page.waitForTimeout(400);
 await page.locator('#modelSearch').fill('fast'); await page.waitForTimeout(850);
 if(!(await page.locator('#modelSearchResults').innerText()).includes('openai/whisper-tiny'))throw Error('stale search');
 if((await page.locator('#modelSearchResults').innerText()).includes('whisper-base'))throw Error('stale result shown');
 if(await page.locator('#modelSearchResults').getByRole('button',{name:'Download',exact:true}).count())throw Error('unsupported download');
 await page.locator('#modelSearch').fill('unsupported'); await page.waitForTimeout(450);
 if((await page.locator('#modelSearchResults').innerText()).includes('org/unsupported'))throw Error('incompatible hub row');
 await page.locator('#modelSearch').fill('whisper'); await page.waitForTimeout(80);
 if(!(await page.locator('#modelSearchListbox').innerText()).includes('Whisper'))throw Error('local filter');
 if(!await page.locator('#modelSearchListbox .publisher-icon').count())throw Error('mini icon');
 await page.waitForTimeout(400);
 await page.locator('#modelSearchResults').getByRole('button',{name:'Choose variant'}).click();
 await page.waitForTimeout(500);
 await page.locator('#modelSearch').fill('official');
 await page.locator('#modelSearchResults').getByText('openai/whisper-tiny', {exact:true}).waitFor();
 await page.locator('#modelSearchResults').getByRole('button',{name:'Choose variant'}).click();
 assert.equal(await variantValue('whisper'), 'tiny');
 await page.locator('#family-whisper').getByRole('button',{name:'Use',exact:true}).click();
 await page.waitForFunction(()=>window.smokeSettings.model==='tiny');
 assert.equal(await page.locator('#modelSelect').inputValue(),'tiny','settings retains library selection');
 await page.locator('#postProcess').dispatchEvent('change');
 await page.waitForTimeout(100);
 assert.equal(await page.evaluate(()=>window.smokeSettings.model),'tiny','an unrelated settings save cannot reset the speech model');
 await page.locator('#modelSearch').fill('parakeet');
 await page.locator('#modelSearchResults').getByText('nvidia/parakeet-tdt-0.6b-v2',{exact:true}).waitFor();
 await page.locator('#modelSearchResults').getByRole('button',{name:'Choose variant'}).click();
 assert.equal(await variantValue('parakeet'), 'parakeet-tdt-0.6b-v2');
 await page.locator('#modelSearch').fill('fail');
 await page.waitForFunction(()=>document.getElementById('modelSearchStatus').textContent.includes('offline'));
 if(process.env.MODEL_SCREENSHOT_PATH) await page.screenshot({path:process.env.MODEL_SCREENSHOT_PATH});
 console.log('PASS families, variant/focus persistence, stale search, compatible-only hub rows, local filter icons, error states, and settings model preservation');
 await browser.close();
})().catch(async error => { console.error(error); if(browser) await browser.close(); process.exitCode=1; });
