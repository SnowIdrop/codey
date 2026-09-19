// Opt-in native CLI probe, launched by the ignored Rust test. Model traffic is loopback-only.
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import zlib from 'node:zlib';
import { spawn } from 'node:child_process';

const root = process.env.CODEY_GEMINI_PROBE_ROOT;
const cli = process.env.CODEY_GEMINI_PROBE_CLI;
const sourceCatalog = process.env.CODEY_GEMINI_PROBE_CATALOG;
if (!root || !cli || !sourceCatalog) throw new Error('Probe paths must be supplied explicitly');
const home=path.join(root,'home'), workspace=path.join(root,'workspace');
fs.mkdirSync(home); fs.mkdirSync(workspace);
const template=fs.readFileSync(new URL('../../resources/gemini-antigravity-base-instructions.md',import.meta.url),'utf8');
const legacy=fs.readFileSync(new URL('../../resources/codex-0.153.3-base-instructions.md',import.meta.url),'utf8');
const catalog=JSON.parse(fs.readFileSync(sourceCatalog,'utf8'));
const parent=structuredClone(catalog.models.find(m=>m.slug.endsWith('/gpt-5.6-terra') || m.slug==='gpt-5.6-terra'));
const gemini=structuredClone(catalog.models.find(m=>m.slug.endsWith('/gemini-3.8-flash-high')));
if (!parent || !gemini) throw new Error('Required catalog models absent');
parent.slug='route-parent/gpt-5.6-terra'; gemini.slug='route-child/gemini-3.8-flash-high';
parent.base_instructions=legacy.trim();
parent.model_messages={instructions_template:legacy.trim(),instructions_variables:null};
fs.writeFileSync(path.join(home,'catalog.json'),JSON.stringify({models:[parent,gemini]}));
fs.writeFileSync(path.join(home,'comments.toml'),`name="codey_comments"\ndescription="Comment-only probe"\nmodel=${JSON.stringify(gemini.slug)}\nmodel_reasoning_effort="high"\ndeveloper_instructions="ROLE_BOUNDARY_SENTINEL: comments only; never change executable code"\nsandbox_mode="read-only"\n`);
const captures=[]; let parentTurns=0; let probeError;
function findTool(tools,name,namespace) {
  for (const tool of tools??[]) {
    if (tool.type==='namespace') { const found=findTool(tool.tools,name,tool.name); if (found) return found; }
    if (tool.name===name) return {namespace};
  }
}
function callTool(body,name,args) {
  const found=findTool(body.tools??body.input?.filter(i=>i.type==='additional_tools').flatMap(i=>i.tools),name);
  if (!found) throw new Error(`Missing native tool: ${name}`);
  return {type:'function_call',id:`fc_${captures.length}`,call_id:`call_${captures.length}`,name,
    ...(found.namespace?{namespace:found.namespace}:{}),arguments:JSON.stringify(args),status:'completed'};
}
function nativeResponse(res,output) {
  const response={id:`resp_${captures.length}`,object:'response',status:'completed',output,usage:{input_tokens:1,output_tokens:1,total_tokens:2}};
  const events=[{type:'response.created',response:{...response,status:'in_progress',output:[]}}];
  output.forEach((item,output_index)=>events.push({type:'response.output_item.added',output_index,item},{type:'response.output_item.done',output_index,item}));
  events.push({type:'response.completed',response});
  res.writeHead(200,{'Content-Type':'text/event-stream','Connection':'close'});
  res.end(events.map(e=>`data: ${JSON.stringify(e)}\n\n`).join(''));
}
const server=http.createServer(async(req,res)=>{
  try {
    const chunks=[];for await(const chunk of req) chunks.push(chunk);
    let bytes=Buffer.concat(chunks);
    if(req.headers['content-encoding']==='zstd')bytes=zlib.zstdDecompressSync(bytes);
    if(req.headers['content-encoding']==='gzip')bytes=zlib.gunzipSync(bytes);
    const body=JSON.parse(bytes.toString()); captures.push({path:req.url,body});
    fs.writeFileSync(path.join(root,`upstream-${captures.length}.json`),JSON.stringify({path:req.url,body},null,2));
    if(body.model==='gemini-3.8-flash-high') {
      res.writeHead(200,{'Content-Type':'text/event-stream','Connection':'close'});
      res.end(`data: ${JSON.stringify({id:'chatcmpl_probe',model:body.model,choices:[{index:0,delta:{role:'assistant',content:'CHILD_PROBE_COMPLETE'},finish_reason:null}]})}\n\ndata: ${JSON.stringify({id:'chatcmpl_probe',choices:[{index:0,delta:{},finish_reason:'stop'}]})}\n\ndata: [DONE]\n\n`);
      return;
    }
    parentTurns++;
    if(parentTurns===1)return nativeResponse(res,[callTool(body,'spawn_agent',{task_name:'gateway_probe_child',agent_type:'codey_comments',fork_turns:'none',message:'TASK_BODY_SENTINEL: preserve strings and docstrings; return CHILD_PROBE_COMPLETE without tools'})]);
    if(parentTurns===2)return nativeResponse(res,[callTool(body,'wait_agent',{timeout_ms:10000})]);
    nativeResponse(res,[{type:'message',id:'msg_parent',role:'assistant',status:'completed',content:[{type:'output_text',text:'PARENT_PROBE_COMPLETE',annotations:[]}]}]);
  }catch(error){probeError=String(error);res.writeHead(500);res.end('local probe failed');}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
fs.writeFileSync(path.join(root,'upstream.json'),JSON.stringify({base_url:`http://127.0.0.1:${server.address().port}/v1`}));
const deadline=Date.now()+30000;
while(!fs.existsSync(path.join(root,'router.json'))) {
  if(Date.now()>deadline)throw new Error('Router handshake timed out');
  await new Promise(resolve=>setTimeout(resolve,50));
}
const endpoint=JSON.parse(fs.readFileSync(path.join(root,'router.json'),'utf8'));
const slash=p=>p.replaceAll('\\','/');
fs.writeFileSync(path.join(home,'config.toml'),`model=${JSON.stringify(parent.slug)}
model_provider="probe"
model_catalog_json=${JSON.stringify(slash(path.join(home,'catalog.json')))}
approval_policy="never"
sandbox_mode="read-only"
web_search="disabled"
[model_providers.probe]
name="Local Codey route probe"
base_url=${JSON.stringify(endpoint.base_url)}
wire_api="responses"
requires_openai_auth=false
request_max_retries=0
stream_max_retries=0
http_headers={ "x-codey-router-token"=${JSON.stringify(endpoint.token)} }
[agents]
enabled=true
max_concurrent_threads_per_session=2
[agents.codey_comments]
description="Comment-only probe"
config_file=${JSON.stringify(slash(path.join(home,'comments.toml')))}
[features.multi_agent_v2]
enabled=true
wait_agent_enabled=true
tool_namespace="agents"
`);
const env=Object.fromEntries(Object.entries(process.env).filter(([k])=>!/^(CODEX|CODEY|OPENAI|ANTHROPIC|GEMINI)/i.test(k)));
env.CODEX_HOME=home;
const child=spawn(cli,['exec','--skip-git-repo-check','--ignore-rules','-C',workspace,'--json','Delegate the sentinel task to codey_comments and wait.'],{env,windowsHide:true,stdio:['ignore','pipe','pipe']});
let stdout='',stderr='';child.stdout.on('data',d=>stdout+=d);child.stderr.on('data',d=>stderr+=d);
const timer=setTimeout(()=>{probeError='Native probe timed out';child.kill();},90000);
const exitCode=await new Promise(resolve=>child.on('close',resolve));clearTimeout(timer);
server.closeAllConnections();await new Promise(resolve=>server.close(resolve));
fs.writeFileSync(path.join(root,'stdout.jsonl'),stdout);fs.writeFileSync(path.join(root,'stderr.log'),stderr);
const childRequests=captures.filter(c=>c.body.model==='gemini-3.8-flash-high');
const normalize=s=>s.replaceAll('\r\n','\n').trim();
const sessions=path.join(home,'sessions');
const childMetadata=fs.existsSync(sessions)?fs.readdirSync(sessions,{recursive:true}).filter(p=>p.endsWith('.jsonl')).map(p=>JSON.parse(fs.readFileSync(path.join(sessions,p),'utf8').split('\n')[0]).payload).filter(m=>m.agent_role==='codey_comments'):[];
const summary={exitCode,probeError,childRequestCount:childRequests.length,
  childPersistedLegacyBase:childMetadata.length>0&&childMetadata.every(m=>normalize(m.base_instructions?.text??'')===normalize(legacy)),
  chatPathCorrect:childRequests.length>0&&childRequests.every(c=>c.path==='/v1/chat/completions'),
  baseMatches:childRequests.length>0&&childRequests.every(c=>normalize(c.body.messages[0].content)===normalize(template)),
  rolePreserved:childRequests.length>0&&childRequests.every(c=>JSON.stringify(c.body.messages).includes('ROLE_BOUNDARY_SENTINEL')),
  taskPreserved:childRequests.length>0&&childRequests.every(c=>JSON.stringify(c.body.messages).includes('TASK_BODY_SENTINEL')),
  toolsPreserved:childRequests.length>0&&childRequests.every(c=>c.body.tools?.length>0),
  reasoningPreserved:childRequests.length>0&&childRequests.every(c=>c.body.reasoning_effort==='high')};
fs.writeFileSync(path.join(root,'summary.json'),JSON.stringify(summary,null,2));console.log(JSON.stringify(summary));
if(exitCode!==0||probeError||Object.entries(summary).some(([k,v])=>k.endsWith('Preserved')||k==='baseMatches'||k==='chatPathCorrect'||k==='childPersistedLegacyBase'?v!==true:false))process.exitCode=1;
