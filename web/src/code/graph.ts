// The commit graph's layout, as git log --graph and IntelliJ draw it: each
// commit on its own row, in a lane, with lines down to its parents. Lanes
// are the commits still to come that rows above point at; a commit takes
// the lane that points at it (the leftmost, others joining it), its first
// parent carries the lane on, and each other parent opens a lane of its
// own unless one already leads there. Freed lanes are reused, so a lane
// keeps its column, and its colour, all the way down.

// An edge is a line within a row. 'in' runs from a lane at the row's top
// to the commit, 'through' passes the row in its lane, and 'out' runs from
// the commit to a lane at the row's bottom, towards a parent.
export type Edge = { kind: 'in' | 'through' | 'out'; from: number; to: number; color: number }

export type GraphRow = { col: number; color: number; edges: Edge[] }

export type Graph = { rows: GraphRow[]; lanes: number }

export function layout(commits: { hash: string; parents: string[] }[]): Graph {
  const lanes: ({ hash: string; color: number } | null)[] = []
  let colors = 0
  let widest = 0
  const free = (not = -1) => {
    const i = lanes.findIndex((l, j) => !l && j !== not)
    return i < 0 ? Math.max(lanes.length, not + 1) : i
  }
  const rows = commits.map((c): GraphRow => {
    const edges: Edge[] = []
    let col = lanes.findIndex((l) => l?.hash === c.hash)
    // A commit no row above points at begins a lane: a branch's tip.
    const color = col < 0 ? colors++ : lanes[col]!.color
    if (col < 0) col = free()
    lanes.forEach((l, i) => {
      if (!l) return
      if (l.hash === c.hash) {
        edges.push({ kind: 'in', from: i, to: col, color: l.color })
        lanes[i] = null
      } else edges.push({ kind: 'through', from: i, to: i, color: l.color })
    })
    c.parents.forEach((p, k) => {
      // The first parent always carries the commit's lane on, even when
      // another lane leads to it too: the lanes meet at the parent, where
      // the branches forked. A merged parent joins a lane that leads there.
      let j = k === 0 ? col : lanes.findIndex((l) => l?.hash === p)
      if (k === 0 || j < 0) {
        if (k > 0) j = free(col)
        lanes[j] = { hash: p, color: k === 0 ? color : colors++ }
      }
      edges.push({ kind: 'out', from: col, to: j, color: lanes[j]!.color })
    })
    while (lanes.length && !lanes[lanes.length - 1]) lanes.pop()
    widest = Math.max(widest, col + 1, lanes.length, ...edges.map((e) => Math.max(e.from, e.to) + 1))
    return { col, color, edges }
  })
  return { rows, lanes: widest }
}
