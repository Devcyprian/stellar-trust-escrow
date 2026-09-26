import { cleanup, render, screen } from '@testing-library/react';
import EscrowStatusBadge from '@/components/escrow/EscrowStatusBadge';

jest.mock('../../../hooks/useEscrowUpdates', () => ({
  useEscrowUpdates: () => ({ status: 'idle', lastPayload: null }),
}));

const statuses = [
  'pending',
  'active',
  'funded',
  'in_dispute',
  'resolved',
  'completed',
  'cancelled',
];

describe('EscrowStatusBadge snapshots', () => {
  it('matches all supported status render states', () => {
    const renderedStatuses = statuses.map((initialStatus) => {
      render(
        <EscrowStatusBadge escrowId={`escrow-${initialStatus}`} initialStatus={initialStatus} />,
      );

      const badge = screen.getByRole('status');
      const snapshot = {
        status: initialStatus,
        text: badge.textContent,
        ariaLabel: badge.getAttribute('aria-label'),
        className: badge.className.replace(/\s+/g, ' ').trim(),
      };
      cleanup();
      return snapshot;
    });

    expect(renderedStatuses).toMatchInlineSnapshot(`
      [
        {
          "ariaLabel": "Escrow status: Pending",
          "className": "inline-flex items-center gap-2 px-3 py-1 rounded-full text-xs font-medium border bg-yellow-900/40 text-yellow-300 border-yellow-700",
          "status": "pending",
          "text": "Pending",
        },
        {
          "ariaLabel": "Escrow status: Active",
          "className": "inline-flex items-center gap-2 px-3 py-1 rounded-full text-xs font-medium border bg-blue-900/40 text-blue-300 border-blue-700",
          "status": "active",
          "text": "Active",
        },
        {
          "ariaLabel": "Escrow status: Funded",
          "className": "inline-flex items-center gap-2 px-3 py-1 rounded-full text-xs font-medium border bg-indigo-900/40 text-indigo-300 border-indigo-700",
          "status": "funded",
          "text": "Funded",
        },
        {
          "ariaLabel": "Escrow status: In Dispute",
          "className": "inline-flex items-center gap-2 px-3 py-1 rounded-full text-xs font-medium border bg-red-900/40 text-red-300 border-red-700",
          "status": "in_dispute",
          "text": "In Dispute",
        },
        {
          "ariaLabel": "Escrow status: Resolved",
          "className": "inline-flex items-center gap-2 px-3 py-1 rounded-full text-xs font-medium border bg-emerald-900/40 text-emerald-300 border-emerald-700",
          "status": "resolved",
          "text": "Resolved",
        },
        {
          "ariaLabel": "Escrow status: Completed",
          "className": "inline-flex items-center gap-2 px-3 py-1 rounded-full text-xs font-medium border bg-green-900/40 text-green-300 border-green-700",
          "status": "completed",
          "text": "Completed",
        },
        {
          "ariaLabel": "Escrow status: Cancelled",
          "className": "inline-flex items-center gap-2 px-3 py-1 rounded-full text-xs font-medium border bg-gray-800/60 text-gray-400 border-gray-600",
          "status": "cancelled",
          "text": "Cancelled",
        },
      ]
    `);
  });
});
