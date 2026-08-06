import { existsSync, readdirSync, readFileSync } from 'node:fs'
import { dirname, extname, relative, resolve, sep } from 'node:path'
import process from 'node:process'

const repositoryRoot = resolve(import.meta.dirname, '..')
const ignoredDirectories = new Set(['.git', '.mineintent', '.artifacts', 'node_modules'])
const failures = []
const requiredProjectDocuments = [
  'README.md',
  'docs/产品.md',
  'docs/README.md',
  'docs/architecture.md',
  'docs/guides/run.md',
  'docs/guides/validation.md',
  'docs/history/index.md',
  'CONTRIBUTING.md',
  'AGENTS.md',
]
const retiredProjectDocuments = [
  'PRODUCT_CONSTITUTION.md',
  '产品.md',
  '产品待澄清问题.md',
  'docs/source-index.md',
  'docs/guides/participant-prototype.md',
  'docs/guides/paper-integration.md',
]

function walkMarkdown(directory) {
  return readdirSync(directory, { withFileTypes: true })
    .flatMap((entry) => {
      if (entry.isDirectory() && ignoredDirectories.has(entry.name)) return []
      const path = resolve(directory, entry.name)
      if (entry.isDirectory()) return walkMarkdown(path)
      return extname(entry.name).toLowerCase() === '.md' ? [path] : []
    })
    .sort()
}

function repoPath(path) {
  return relative(repositoryRoot, path).split(sep).join('/')
}

function withoutFencedCode(source) {
  const lines = source.split(/\r?\n/)
  let fence = null
  return lines.map((line) => {
    const marker = line.match(/^\s*(`{3,}|~{3,})/)
    if (marker) {
      if (!fence) fence = marker[1][0]
      else if (marker[1][0] === fence) fence = null
      return ''
    }
    return fence ? '' : line
  }).join('\n')
}

function markdownDestinations(source) {
  const text = withoutFencedCode(source)
  const destinations = []
  const inline = /!?\[[^\]]*\]\(\s*(?:<([^>]+)>|([^\s)]+))(?:\s+(?:"[^"]*"|'[^']*'|\([^)]*\)))?\s*\)/g
  const definitions = /^\s{0,3}\[[^\]]+\]:\s*(?:<([^>]+)>|(\S+))/gm
  for (const pattern of [inline, definitions]) {
    for (const match of text.matchAll(pattern)) destinations.push(match[1] ?? match[2])
  }
  return destinations
}

function resolveLocalDestination(sourcePath, destination) {
  if (!destination || destination.startsWith('#')) return null
  if (/^[a-z][a-z\d+.-]*:/i.test(destination) || destination.startsWith('//')) return null
  const pathPart = destination.split('#', 1)[0].split('?', 1)[0]
  if (!pathPart) return null
  try {
    const decoded = decodeURIComponent(pathPart)
    return decoded.startsWith('/')
      ? resolve(repositoryRoot, `.${decoded}`)
      : resolve(dirname(sourcePath), decoded)
  } catch {
    failures.push(`${repoPath(sourcePath)}: 链接含无效 URL 编码 ${JSON.stringify(destination)}`)
    return null
  }
}

function checkDocument(path) {
  const source = readFileSync(path, 'utf8')
  if (/github\.com\/spojchil\/maineintent\b/i.test(source)) {
    failures.push(`${repoPath(path)}: 仍引用旧仓库名 spojchil/maineintent`)
  }
  for (const destination of markdownDestinations(source)) {
    const target = resolveLocalDestination(path, destination)
    if (!target) continue
    if (!target.startsWith(`${repositoryRoot}${sep}`) && target !== repositoryRoot) {
      failures.push(`${repoPath(path)}: 本地链接越出仓库 ${JSON.stringify(destination)}`)
    } else if (!existsSync(target)) {
      failures.push(`${repoPath(path)}: 断裂链接 ${JSON.stringify(destination)}`)
    }
  }
}

const documents = walkMarkdown(repositoryRoot)
for (const path of requiredProjectDocuments) {
  if (!existsSync(resolve(repositoryRoot, path))) failures.push(`缺少项目文档入口 ${path}`)
}
for (const path of retiredProjectDocuments) {
  if (existsSync(resolve(repositoryRoot, path))) failures.push(`旧项目文档入口不应恢复 ${path}`)
}
for (const path of documents) checkDocument(path)

if (failures.length) {
  console.error(`文档检查失败（${failures.length} 项）：`)
  for (const failure of failures) console.error(`- ${failure}`)
  process.exitCode = 1
} else {
  console.log(`文档检查通过：${documents.length} 份 Markdown，本地链接有效。`)
}
