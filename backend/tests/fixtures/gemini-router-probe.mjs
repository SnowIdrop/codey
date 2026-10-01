// Opt-in native CLI probe, launched by the ignored Rust test. Model traffic is loopback-only.
import fs from 'node:fs';
import path from 'node:path';
import http from 'node:http';
import zlib from 'node:zlib';
import crypto from 'node:crypto';
import { spawn } from 'node:child_process';

const root = process.env.CODEY_GEMINI_PROBE_ROOT;
const cli = process.env.CODEY_GEMINI_PROBE_CLI;
const sourceCatalog = process.env.CODEY_GEMINI_PROBE_CATALOG;
const gate = process.env.CODEY_GEMINI_PROBE_GATE;
const baseline = process.env.CODEY_GEMINI_PROBE_BASELINE ?? 'gpt6';
const rejected = process.env.CODEY_GEMINI_PROBE_REJECTED === '1';
const payloadMode = process.env.CODEY_GEMINI_PROBE_PAYLOAD ?? 'plaintext';
const schemaShape = process.env.CODEY_GEMINI_PROBE_SCHEMA_SHAPE ?? 'top';
if (!['plaintext','ciphertext','schema'].includes(payloadMode)) throw new Error('Unsupported probe task payload');
if (!['top','children','additional','search'].includes(schemaShape)) throw new Error('Unsupported probe schema shape');
const expectedTaskBodyUnavailable = payloadMode === 'ciphertext';
const plaintextTask = 'TASK_BODY_SENTINEL: preserve strings and docstrings; return CHILD_PROBE_COMPLETE without tools';
const syntheticCiphertext = Buffer.alloc(73);
syntheticCiphertext[0] = 0x80;
const taskPayload = expectedTaskBodyUnavailable ? syntheticCiphertext.toString('base64url') : plaintextTask;
if (!root || !cli || !sourceCatalog || !gate) throw new Error('Probe paths must be supplied explicitly');
const home=path.join(root,'home'), workspace=path.join(root,'workspace');
fs.mkdirSync(home); fs.mkdirSync(workspace);
const template=fs.readFileSync(new URL('../../resources/gemini-antigravity-base-instructions.md',import.meta.url),'utf8');
const legacy=fs.readFileSync(new URL('../../resources/codex-0.153.3-base-instructions.md',import.meta.url),'utf8');
const gpt6=fs.readFileSync(new URL('../../resources/codex-0.155.0-alpha.9-gpt6-base-instructions.md',import.meta.url),'utf8');
const codingAgent=fs.readFileSync(new URL('../../resources/codex-0.153.3-coding-agent-base-instructions.md',import.meta.url),'utf8');
const childRole=baseline==='deepseek'?'codey_worker':['coding-agent','catalog'].includes(baseline)?'codey_quick_scan':'codey_comments';
const roleInput=JSON.parse(process.env.CODEY_GEMINI_PROBE_ROLE_INPUT ?? JSON.stringify({agent_type:childRole}));
const childModel=baseline==='deepseek'?'deepseek-flash':'gemini-3.8-flash-high';
const childEffort=baseline==='deepseek'?'max':'high';
const catalog=JSON.parse(fs.readFileSync(sourceCatalog,'utf8'));
const parentEntry=['gpt-5.6-terra','gpt-6.1-sol'].map(slug=>catalog.models.find(m=>m.slug.endsWith('/'+slug)||m.slug===slug)).find(Boolean);
const parent=structuredClone(parentEntry);
const gemini=structuredClone(catalog.models.find(m=>m.slug.endsWith('/'+childModel)));
if (!parent || !gemini) throw new Error('Required catalog models absent');
const parentModel=parent.slug.split('/').at(-1);
const overriddenBases={legacy,gpt6,'coding-agent':codingAgent,deepseek:gpt6};
const parentBase=baseline==='catalog'?parent.base_instructions:overriddenBases[baseline];
if(typeof parentBase!=='string'||!parentBase.trim())throw new Error('Parent catalog base instructions absent');
parent.slug='route-parent/'+parentModel; gemini.slug='route-child/'+childModel;
if(baseline!=='catalog') {
  parent.base_instructions=parentBase.trim();
  parent.model_messages={instructions_template:parentBase.trim(),instructions_variables:null};
}
fs.writeFileSync(path.join(home,'catalog.json'),JSON.stringify({models:[parent,gemini]}));
fs.writeFileSync(path.join(home,'comments.toml'),`name=${JSON.stringify(childRole)}\ndescription="Native role probe"\nmodel=${JSON.stringify(gemini.slug)}\nmodel_reasoning_effort=${JSON.stringify(childEffort)}\ndeveloper_instructions="ROLE_BOUNDARY_SENTINEL: return evidence only; never change files in this probe"\nsandbox_mode="read-only"\n`);
// Record the real native payload, then delegate its decision to the built Codey hook.
const hookScript=path.join(root,'hook.mjs');
fs.writeFileSync(hookScript,`import fs from 'node:fs';import {spawnSync} from 'node:child_process';
const raw=fs.readFileSync(0,'utf8');
fs.appendFileSync(${JSON.stringify(path.join(root,'hooks.jsonl'))},raw.trim()+'\\n');
const result=spawnSync(${JSON.stringify(gate)},['--codey-subagent-gate-hook'],{input:raw,encoding:'utf8',windowsHide:true});
fs.appendFileSync(${JSON.stringify(path.join(root,'hook-results.jsonl'))},JSON.stringify({status:result.status,output:result.stdout,error:result.stderr})+'\\n');
process.stdout.write(result.stdout||'{}');process.exitCode=result.status??1;`);
const hookCommand=`node '${hookScript.replaceAll("'","''")}'`;
const hooksPath=path.join(home,'hooks.json');
fs.writeFileSync(hooksPath,JSON.stringify({hooks:{PreToolUse:[{matcher:'*',hooks:[{type:'command',command:hookCommand,commandWindows:hookCommand,timeout:5}]}]}}));
const identity={event_name:'pre_tool_use',hooks:[{async:false,command:hookCommand,timeout:5,type:'command'}],matcher:'*'};
const trustedHash='sha256:'+crypto.createHash('sha256').update(JSON.stringify(identity)).digest('hex');
const captures=[]; let parentTurns=0; let probeError; let producedTaskFormat;
function nativeTools(body) {
  const input=Array.isArray(body.input)?body.input:[body.input];
  return [...body.tools??[],...input.filter(i=>i&&(i.type==='additional_tools'||i.type==='tool_search_output'&&i.execution==='client')).flatMap(i=>i.tools??[])];
}
function findTool(tools,name,namespace) {
  for (const tool of tools??[]) {
    if (tool.type==='namespace') { const found=findTool([...tool.tools??[],...tool.children??[]],name,tool.name); if (found) return found; }
    if (tool.name===name) return {namespace,tool};
  }
}
function callTool(body,name,args) {
  const found=findTool(nativeTools(body),name);
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
    if(body.model===childModel) {
      if(baseline==='deepseek')return nativeResponse(res,[{type:'message',id:'msg_child',role:'assistant',status:'completed',content:[{type:'output_text',text:'CHILD_PROBE_COMPLETE',annotations:[]}]}]);
      res.writeHead(200,{'Content-Type':'text/event-stream','Connection':'close'});
      res.end(`data: ${JSON.stringify({id:'chatcmpl_probe',model:body.model,choices:[{index:0,delta:{role:'assistant',content:'CHILD_PROBE_COMPLETE'},finish_reason:null}]})}\n\ndata: ${JSON.stringify({id:'chatcmpl_probe',choices:[{index:0,delta:{},finish_reason:'stop'}]})}\n\ndata: [DONE]\n\n`);
      return;
    }
    parentTurns++;
    if(parentTurns===1) {
      const encrypted=payloadMode==='ciphertext'||payloadMode==='schema'&&findTool(nativeTools(body),'spawn_agent')?.tool.parameters?.properties?.message?.encrypted===true;
      producedTaskFormat=encrypted?'ciphertext':'plaintext';
      return nativeResponse(res,[callTool(body,'spawn_agent',{task_name:'gateway_probe_child',...roleInput,fork_turns:'none',message:encrypted?syntheticCiphertext.toString('base64url'):taskPayload})]);
    }
    if(parentTurns===2&&!rejected)return nativeResponse(res,[callTool(body,'wait_agent',{timeout_ms:10000})]);
    nativeResponse(res,[{type:'message',id:'msg_parent',role:'assistant',status:'completed',content:[{type:'output_text',text:'PARENT_PROBE_COMPLETE',annotations:[]}]}]);
  }catch(error){probeError=String(error);res.writeHead(500);res.end('local probe failed');}
});
await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
fs.writeFileSync(path.join(root,'upstream.json'),JSON.stringify({base_url:`http://127.0.0.1:${server.address().port}/v1`,parent_model:parentModel}));
const deadline=Date.now()+30000;
while(!fs.existsSync(path.join(root,'router.json'))) {
  if(Date.now()>deadline)throw new Error('Router handshake timed out');
  await new Promise(resolve=>setTimeout(resolve,50));
}
const endpoint=JSON.parse(fs.readFileSync(path.join(root,'router.json'),'utf8'));
const schemaIngress=http.createServer(async(req,res)=>{
  try {
    const chunks=[];for await(const chunk of req)chunks.push(chunk);
    let bytes=Buffer.concat(chunks);
    if(req.headers['content-encoding']==='zstd')bytes=zlib.zstdDecompressSync(bytes);
    if(req.headers['content-encoding']==='gzip')bytes=zlib.gunzipSync(bytes);
    const body=JSON.parse(bytes.toString());
    if((body.model===parent.slug||body.model===parentModel)&&schemaShape!=='top') {
      const agents=body.tools?.find(tool=>tool.type==='namespace'&&tool.name==='agents');
      if(agents) {
        if(schemaShape==='children') {agents.children=agents.tools;delete agents.tools;}
        else {
          body.tools=body.tools.filter(tool=>tool!==agents);
          const input=Array.isArray(body.input)?body.input:[body.input];
          const definitions=schemaShape==='additional'
            ?[{type:'additional_tools',role:'developer',tools:[agents]}]
            :[{type:'tool_search_call',execution:'client',call_id:'call_probe_search',arguments:{goal:'load collaboration tools'}},
              {type:'tool_search_output',execution:'client',call_id:'call_probe_search',tools:[agents]}];
          body.input=[...definitions,...input];
        }
      }
    }
    const headers=Object.fromEntries(Object.entries(req.headers).filter(([key])=>!['host','connection','content-length','content-encoding'].includes(key)));
    const response=await fetch(endpoint.base_url.replace(/\/v1\/?$/,'')+req.url,{method:'POST',headers,body:JSON.stringify(body)});
    res.writeHead(response.status,{'Content-Type':response.headers.get('content-type')??'application/json'});
    for await(const chunk of response.body)res.write(chunk);
    res.end();
  }catch(error){probeError=String(error);res.writeHead(500);res.end('local schema ingress failed');}
});
await new Promise(resolve=>schemaIngress.listen(0,'127.0.0.1',resolve));
const cliBaseURL=`http://127.0.0.1:${schemaIngress.address().port}/v1`;
const slash=p=>p.replaceAll('\\','/');
fs.writeFileSync(path.join(home,'config.toml'),`model=${JSON.stringify(parent.slug)}
model_provider="probe"
model_catalog_json=${JSON.stringify(slash(path.join(home,'catalog.json')))}
approval_policy="never"
sandbox_mode="read-only"
web_search="disabled"
[features]
hooks=true
[hooks.state.${JSON.stringify(hooksPath+':pre_tool_use:0:0')}]
trusted_hash=${JSON.stringify(trustedHash)}
[model_providers.probe]
name="Local Codey route probe"
base_url=${JSON.stringify(cliBaseURL)}
wire_api="responses"
requires_openai_auth=false
request_max_retries=0
stream_max_retries=0
http_headers={ "x-codey-router-token"=${JSON.stringify(endpoint.token)} }
[agents]
enabled=true
max_concurrent_threads_per_session=2
[agents.${childRole}]
description="Native role probe"
config_file=${JSON.stringify(slash(path.join(home,'comments.toml')))}
[features.multi_agent_v2]
enabled=true
wait_agent_enabled=true
tool_namespace="agents"
multi_agent_mode_hint_text=${JSON.stringify(endpoint.role_hint)}
`);
const env=Object.fromEntries(Object.entries(process.env).filter(([k])=>!/^(CODEX|CODEY|OPENAI|ANTHROPIC|GEMINI)/i.test(k)));
env.CODEX_HOME=home;
env.CODEY_APP_STATE_DIR=path.join(root,'state');
env.CODEY_SUBAGENT_GATE_ACTIVE='1';
env.CODEY_SUBAGENT_GATE_RUNTIME_ID='isolated-native-probe';
const child=spawn(cli,['exec','--skip-git-repo-check','--ignore-rules','-C',workspace,'--json',`Delegate the sentinel task to ${childRole} and wait.`],{env,windowsHide:true,stdio:['ignore','pipe','pipe']});
let stdout='',stderr='';child.stdout.on('data',d=>stdout+=d);child.stderr.on('data',d=>stderr+=d);
const timer=setTimeout(()=>{probeError='Native probe timed out';child.kill();},90000);
const exitCode=await new Promise(resolve=>child.on('close',resolve));clearTimeout(timer);
schemaIngress.closeAllConnections();await new Promise(resolve=>schemaIngress.close(resolve));
server.closeAllConnections();await new Promise(resolve=>server.close(resolve));
fs.writeFileSync(path.join(root,'stdout.jsonl'),stdout);fs.writeFileSync(path.join(root,'stderr.log'),stderr);
const childRequests=captures.filter(c=>c.body.model===childModel);
const normalize=s=>s.replaceAll('\r\n','\n').trim();
const sessions=path.join(home,'sessions');
const childLogs=fs.existsSync(sessions)?fs.readdirSync(sessions,{recursive:true}).filter(p=>p.endsWith('.jsonl')).map(p=>fs.readFileSync(path.join(sessions,p),'utf8').trim().split('\n').map(JSON.parse)).filter(rows=>rows[0].payload.parent_thread_id||rows[0].payload.source?.subagent):[];
const childMetadata=childLogs.map(rows=>rows[0].payload);
const hookInputs=fs.existsSync(path.join(root,'hooks.jsonl'))?fs.readFileSync(path.join(root,'hooks.jsonl'),'utf8').trim().split('\n').map(JSON.parse):[];
const hookResults=fs.existsSync(path.join(root,'hook-results.jsonl'))?fs.readFileSync(path.join(root,'hook-results.jsonl'),'utf8').trim().split('\n').map(JSON.parse):[];
const tools=captures[0]?nativeTools(captures[0].body):[];
const spawnSchema=findTool(tools,'spawn_agent')?.tool.parameters;
const roleRejected=hookResults.some(r=>r.output?.includes('CODEY_SUBAGENT_ROLE_')&&r.output.includes('deny'));
const taskBodyUnavailable=childLogs.some(rows=>JSON.stringify(rows).includes('agent_task_body_unavailable'))||stdout.includes('agent_task_body_unavailable')||stderr.includes('agent_task_body_unavailable')||captures.some(c=>JSON.stringify(c.body).includes('agent_task_body_unavailable'));
const summary={baseline,parentModel,roleInput,payloadMode,schemaShape,producedTaskFormat,expectedTaskBodyUnavailable,expectedRejection:rejected,exitCode,probeError,childRequestCount:childRequests.length,
  plaintextSchemaRequested:spawnSchema?.properties?.message?.encrypted!==true,
  taskBodyUnavailable,
  noChildUpstreamRequest:childRequests.length===0,
  catalogBaseUnmodified:baseline!=='catalog'||(parent.base_instructions===parentEntry.base_instructions&&JSON.stringify(parent.model_messages)===JSON.stringify(parentEntry.model_messages)),
  childCompleted:childLogs.length>0&&childLogs.every(rows=>rows.some(r=>r.type==='event_msg'&&r.payload.type==='task_complete'&&r.payload.last_agent_message?.includes('CHILD_PROBE_COMPLETE'))),
  hookSawSpawn:hookInputs.some(i=>i.tool_name?.endsWith('spawn_agent')),
  roleSchemaRestricted:JSON.stringify(spawnSchema?.properties?.agent_type?.enum)===JSON.stringify([childRole])&&spawnSchema?.required?.includes('agent_type'),
  hintComplete:captures.some(c=>JSON.stringify(c.body.input).includes(JSON.stringify(endpoint.role_hint).slice(1,-1))),
  roleRejected,noChildCreated:childMetadata.length===0,
  childPersistedKnownBase:childMetadata.length>0&&childMetadata.every(m=>normalize(m.base_instructions?.text??'')===normalize(parentBase)),
  childRuntimeMatches:childLogs.length>0&&childLogs.every(rows=>rows.some(r=>r.type==='turn_context'&&r.payload.model===gemini.slug&&r.payload.effort===childEffort)),
  chatPathCorrect:childRequests.length>0&&childRequests.every(c=>c.path===(baseline==='deepseek'?'/v1/responses':'/v1/chat/completions')),
  baseMatches:childRequests.length>0&&childRequests.every(c=>normalize(baseline==='deepseek'?c.body.instructions:c.body.messages[0].content)===normalize(baseline==='deepseek'?parentBase:template)),
  rolePreserved:childRequests.length>0&&childRequests.every(c=>JSON.stringify(c.body.messages??c.body.input).includes('ROLE_BOUNDARY_SENTINEL')),
  taskPreserved:childRequests.length>0&&childRequests.every(c=>JSON.stringify(c.body.messages??c.body.input).includes('TASK_BODY_SENTINEL')),
  toolsPreserved:childRequests.length>0&&childRequests.every(c=>c.body.tools?.length>0),
  reasoningPreserved:childRequests.length>0&&childRequests.every(c=>(c.body.reasoning_effort??c.body.reasoning?.effort)===childEffort)};
fs.writeFileSync(path.join(root,'summary.json'),JSON.stringify(summary,null,2));console.log(JSON.stringify(summary));
const required=rejected?['hookSawSpawn','roleSchemaRestricted','hintComplete','roleRejected','noChildCreated']:expectedTaskBodyUnavailable?['taskBodyUnavailable','noChildUpstreamRequest','hookSawSpawn','roleSchemaRestricted','hintComplete','childPersistedKnownBase','childRuntimeMatches']:['catalogBaseUnmodified','childCompleted','hookSawSpawn','roleSchemaRestricted','hintComplete','childPersistedKnownBase','childRuntimeMatches','chatPathCorrect','baseMatches','rolePreserved','taskPreserved','toolsPreserved','reasoningPreserved'];
if(exitCode!==0||probeError||required.some(key=>summary[key]!==true))process.exitCode=1;
