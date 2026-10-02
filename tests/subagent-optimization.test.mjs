import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);

test("copywriter instructions cover writing tasks without widening behavioral authority", async () => {
  const source = await readFile(new URL("customizations/codex-constraints/agents/codey_copywriter.toml", root), "utf8");
  for (const required of [
    'name = "codey_copywriter"',
    'sandbox_mode = "workspace-write"',
    "目标读者", "文字用途", "事实或设定依据", "不可变项",
    "源码注释", "技术文档", "界面文字", "日志与错误消息", "国家设定", "叙事",
    "区分已有设定与新增创作", "不把创作自由用于技术说明",
    "保持占位符", "插值表达式", "结构化键", "事件码", "已知解析依赖不变",
    "交回主代理", "不自行调整解析器、断言或快照",
    "明确委派时可撰写提示词、技能说明和代理指令",
    "不自行改变规则", "不派生其他代理", "程序契约保持不变",
  ]) assert.ok(source.includes(required), required);
  assert.doesNotMatch(source, /comment-only|只处理手写源码|不得修改字符串/);
});

test("subagent settings expose the six supported role controls", async () => {
  const [featurePolicySource, modelHookSource, modelOptionsSource, comboboxSource] = await Promise.all([
    readFile(new URL("src/FeaturePolicyCard.tsx", root), "utf8"),
    readFile(new URL("src/useModelSelection.ts", root), "utf8"),
    readFile(new URL("src/subagentModels.ts", root), "utf8"),
    readFile(new URL("src/components/ModelCombobox.tsx", root), "utf8"),
  ]);

  assert.match(featurePolicySource, /checked=\{config\.subagentOptimization\}/);
  assert.match(
    featurePolicySource,
    /onCheckedChange=\{\(checked\) =>\s*onSubagentOptimizationChange\(checked\)\s*\}/,
  );
  for (const [id, name] of [
    ["codey_quick_scan", "快速定位"],
    ["codey_deep_research", "深度检索"],
    ["codey_visual_analysis", "视觉分析"],
    ["codey_worker", "代码实施"],
    ["codey_copywriter", "文案"],
    ["codey_visual_worker", "视觉实施"],
  ]) {
    assert.match(featurePolicySource, new RegExp(`id: "${id}"`));
    assert.match(featurePolicySource, new RegExp(`name: "${name}"`));
  }
  assert.match(featurePolicySource, /config\.subagentRoles\[task\.id\]/);
  assert.match(featurePolicySource, /checked=\{selection\.enabled\}/);
  assert.match(featurePolicySource, /onCheckedChange=\{\(enabled\) => updateRole\(\{ enabled \}\)\}/);
  assert.match(featurePolicySource, /"可写"/);
  assert.match(featurePolicySource, /"只读"/);
  assert.match(featurePolicySource, /WRITABLE_SUBAGENT_ROLE_IDS\s*=\s*\[[\s\S]*?"codey_copywriter"/);
  assert.ok(featurePolicySource.indexOf('id: "codey_worker"') < featurePolicySource.indexOf('id: "codey_copywriter"'));
  assert.ok(featurePolicySource.indexOf('id: "codey_copywriter"') < featurePolicySource.indexOf('id: "codey_visual_worker"'));
  assert.match(featurePolicySource, /roleDisabled/);
  assert.match(featurePolicySource, /enabledReadOnlyRoleNames/);
  assert.match(featurePolicySource, /请先启用至少一个只读角色/);
  assert.match(featurePolicySource, /<ModelCombobox/);
  assert.match(
    modelHookSource,
    /buildSubagentModelOptions\(\s*config,\s*modelState,\s*officialAccountAvailable/,
  );
  assert.match(modelHookSource, /officialAccountAvailable,\s*currentProvider/);
  assert.match(modelOptionsSource, /for \(const profile of config\.profiles\)/);
  assert.match(modelOptionsSource, /value: routeModelAlias\(profile, modelId\)/);
  assert.match(modelOptionsSource, /if \(!config\.localRouterEnabled\)/);
  assert.match(modelOptionsSource, /currentProvider\?\.id/);
  assert.match(modelOptionsSource, /value: modelId/);
  assert.match(modelOptionsSource, /: modelState\.thirdPartyModels/);
  assert.match(modelOptionsSource, /official && officialModelMetadata/);
  assert.match(
    modelOptionsSource,
    /THIRD_PARTY_REASONING_EFFORTS\s*=\s*\["low",\s*"medium",\s*"high",\s*"xhigh"\]/,
  );
  assert.match(
    modelOptionsSource,
    /THIRD_PARTY_REASONING_EFFORT_ALLOWLIST\s*=\s*\[\s*"low",\s*"medium",\s*"high",\s*"xhigh",\s*"max",\s*"ultra"/,
  );
  assert.match(modelOptionsSource, /modelState\.thirdPartyModelMetadata/);
  assert.match(modelOptionsSource, /resolveSubagentModelOption/);
  assert.match(comboboxSource, /<ComboBox/);
  assert.match(comboboxSource, /<ListBox\.Section/);
  assert.match(comboboxSource, /没有匹配的模型或线路/);
  assert.match(comboboxSource, /items=\{groups\}/);
});
