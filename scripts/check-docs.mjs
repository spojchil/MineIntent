import { existsSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, extname, join, relative, resolve, sep } from 'node:path'
import process from 'node:process'

const repositoryRoot = resolve(import.meta.dirname, '..')
const docsRoot = join(repositoryRoot, 'docs')
const registerPath = join(docsRoot, 'document-register.md')

const allowed = {
  status: new Set(['accepted', 'proposed', 'experimental', 'historical', 'reference']),
  authority: new Set(['normative', 'informative']),
  implementation: new Set([
    'current',
    'partial',
    'planned',
    'stalled',
    'diverged',
    'retired',
    'not-applicable',
  ]),
}

const linkEntryPoints = [
  join(repositoryRoot, 'README.md'),
  join(repositoryRoot, 'CONTRIBUTING.md'),
  join(repositoryRoot, 'companion-profile.md'),
  join(repositoryRoot, '.github', 'PULL_REQUEST_TEMPLATE.md'),
  join(repositoryRoot, 'agent-service', 'README.md'),
  join(repositoryRoot, 'mcserver', 'README.md'),
]

const failures = []
const metadataByPath = new Map()

function walkMarkdown(directory) {
  return readdirSync(directory, { withFileTypes: true })
    .flatMap((entry) => {
      const path = join(directory, entry.name)
      return entry.isDirectory() ? walkMarkdown(path) : extname(entry.name) === '.md' ? [path] : []
    })
    .sort()
}

function repoPath(path) {
  return relative(repositoryRoot, path).split(sep).join('/')
}

function parseFrontMatter(path, source) {
  const match = source.match(/^---\r?\n([\s\S]*?)\r?\n---(?:\r?\n|$)/)
  if (!match) {
    failures.push(`${repoPath(path)}: 缺少文件开头的 YAML front matter`)
    return null
  }

  const metadata = new Map()
  for (const line of match[1].split(/\r?\n/)) {
    if (!line.trim() || line.trimStart().startsWith('#')) continue
    const field = line.match(/^([a-z_]+):\s*(.+?)\s*$/)
    if (!field) {
      failures.push(`${repoPath(path)}: 无法解析元数据行 ${JSON.stringify(line)}`)
      continue
    }
    metadata.set(field[1], field[2])
  }

  for (const [key, values] of Object.entries(allowed)) {
    const value = metadata.get(key)
    if (!value) failures.push(`${repoPath(path)}: 缺少 ${key}`)
    else if (!values.has(value)) failures.push(`${repoPath(path)}: ${key} 使用未知值 ${JSON.stringify(value)}`)
  }

  const lastVerified = metadata.get('last_verified')
  if (!lastVerified) failures.push(`${repoPath(path)}: 缺少 last_verified`)
  else if (!/^\d{4}-\d{2}-\d{2}$/.test(lastVerified)) {
    failures.push(`${repoPath(path)}: last_verified 必须是 YYYY-MM-DD`)
  }

  // A baseline is "branch@sha" or a bare "sha"; several baselines may be joined with " + ".
  const baseline = /^[0-9a-f]{7,40}$|^[\w./-]+@[0-9a-f]{7,40}$/
  for (const key of ['applies_to', 'source_commit']) {
    const value = metadata.get(key)
    if (value && !value.split(' + ').every((part) => baseline.test(part.trim()))) {
      failures.push(`${repoPath(path)}: ${key} 必须是 分支@短提交 或 短提交，实为 ${JSON.stringify(value)}`)
    }
  }
  if (metadata.get('status') === 'experimental' && !metadata.get('applies_to')) {
    failures.push(`${repoPath(path)}: experimental 文档必须用 applies_to 指明基线`)
  }

  if (
    ['proposed', 'experimental', 'historical'].includes(metadata.get('status')) &&
    metadata.get('authority') !== 'informative'
  ) {
    failures.push(`${repoPath(path)}: ${metadata.get('status')} 文档不能声明 normative authority`)
  }

  return metadata
}

function withoutFencedCode(source) {
  const lines = source.split(/\r?\n/)
  let fence = null
  return lines
    .map((line) => {
      const marker = line.match(/^\s*(`{3,}|~{3,})/)
      if (marker) {
        if (!fence) fence = marker[1][0]
        else if (marker[1][0] === fence) fence = null
        return ''
      }
      return fence ? '' : line
    })
    .join('\n')
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

  let decoded
  try {
    decoded = decodeURIComponent(pathPart)
  } catch {
    failures.push(`${repoPath(sourcePath)}: 链接含无效的 URL 编码 ${JSON.stringify(destination)}`)
    return null
  }

  return decoded.startsWith('/')
    ? resolve(repositoryRoot, `.${decoded}`)
    : resolve(dirname(sourcePath), decoded)
}

function checkLinks(path, source) {
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

const docs = walkMarkdown(docsRoot)
for (const path of docs) {
  const source = readFileSync(path, 'utf8')
  const metadata = parseFrontMatter(path, source)
  if (metadata) metadataByPath.set(path, metadata)
  checkLinks(path, source)
}

for (const path of linkEntryPoints) {
  if (!existsSync(path)) continue
  checkLinks(path, readFileSync(path, 'utf8'))
}

if (!existsSync(registerPath)) {
  failures.push('docs/document-register.md: 文档登记表不存在')
} else {
  const registerSource = readFileSync(registerPath, 'utf8')
  const registeredTargets = new Set()
  const rowPattern = /^\| \[[^\]]+\]\(([^)]+)\) \| ([a-z-]+) \/ ([a-z-]+) \| ([a-z-]+) \|/gm
  for (const match of registerSource.matchAll(rowPattern)) {
    const target = resolveLocalDestination(registerPath, match[1])
    if (!target || !existsSync(target) || !statSync(target).isFile()) continue
    registeredTargets.add(target)
    const metadata = metadataByPath.get(target)
    if (!metadata) continue
    for (const [key, registered] of [
      ['status', match[2]],
      ['authority', match[3]],
      ['implementation', match[4]],
    ]) {
      if (metadata.get(key) !== registered) {
        failures.push(
          `${repoPath(target)}: 登记表 ${key}=${registered}，front matter 为 ${metadata.get(key)}`,
        )
      }
    }
  }
  for (const path of docs) {
    if (path !== registerPath && !registeredTargets.has(path)) {
      failures.push(`${repoPath(path)}: 未列入 docs/document-register.md`)
    }
  }
}

if (failures.length > 0) {
  console.error(`文档检查失败（${failures.length} 项）：`)
  for (const failure of failures) console.error(`- ${failure}`)
  process.exitCode = 1
} else {
  console.log(`文档检查通过：${docs.length} 份 docs 文档，链接与元数据有效。`)
}
