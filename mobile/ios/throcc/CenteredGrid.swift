import SwiftUI

/// As many columns as fit items at least `minimumWidth` wide, every row centred.
struct CenteredGrid: Layout {
    let minimumWidth: CGFloat
    var maximumWidth: CGFloat = .infinity
    let spacing: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) -> CGSize {
        let (itemWidth, rows) = rows(width: proposal.width, subviews: subviews)
        let width = rows.map { rowWidth($0.items.count, itemWidth: itemWidth) }.max() ?? 0
        let height = rows.map(\.height).reduce(0, +) + spacing * CGFloat(max(rows.count - 1, 0))
        return CGSize(width: width, height: height)
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout ()) {
        let (itemWidth, rows) = rows(width: bounds.width, subviews: subviews)
        var y = bounds.minY
        for row in rows {
            var x = bounds.midX - rowWidth(row.items.count, itemWidth: itemWidth) / 2
            for index in row.items {
                subviews[index].place(
                    at: CGPoint(x: x, y: y),
                    proposal: ProposedViewSize(width: itemWidth, height: row.height)
                )
                x += itemWidth + spacing
            }
            y += row.height + spacing
        }
    }

    private func rowWidth(_ count: Int, itemWidth: CGFloat) -> CGFloat {
        CGFloat(count) * itemWidth + spacing * CGFloat(max(count - 1, 0))
    }

    private func rows(width: CGFloat?, subviews: Subviews) -> (itemWidth: CGFloat, rows: [Row]) {
        let width = width ?? CGFloat(subviews.count) * (minimumWidth + spacing)
        let columns = max(1, Int((width + spacing) / (minimumWidth + spacing)))
        let itemWidth = min(maximumWidth, (width - spacing * CGFloat(columns - 1)) / CGFloat(columns))
        let rows = stride(from: 0, to: subviews.count, by: columns).map { start in
            let items = Array(start..<min(start + columns, subviews.count))
            let height = items.map { subviews[$0].sizeThatFits(ProposedViewSize(width: itemWidth, height: nil)).height }
            return Row(items: items, height: height.max() ?? 0)
        }
        return (itemWidth, rows)
    }

    private struct Row {
        let items: [Int]
        let height: CGFloat
    }
}
