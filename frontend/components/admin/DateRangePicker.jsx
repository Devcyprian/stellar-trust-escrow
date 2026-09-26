'use client';

import { CalendarDays } from 'lucide-react';

export default function DateRangePicker({
  value = { from: '', to: '' },
  onChange,
  idPrefix = 'admin-date-range',
}) {
  const fromId = `${idPrefix}-from`;
  const toId = `${idPrefix}-to`;

  const update = (field, nextValue) => {
    onChange?.({ ...value, [field]: nextValue });
  };

  return (
    <fieldset className="rounded-lg border border-gray-200 bg-white px-4 py-3 dark:border-gray-800 dark:bg-gray-950">
      <legend className="flex items-center gap-2 px-1 text-sm font-semibold text-gray-900 dark:text-gray-100">
        <CalendarDays aria-hidden="true" className="h-4 w-4 text-indigo-600 dark:text-indigo-400" />
        Date range
      </legend>
      <div className="mt-3 grid gap-3 sm:grid-cols-2">
        <div>
          <label
            htmlFor={fromId}
            className="mb-1 block text-xs font-medium text-gray-600 dark:text-gray-400"
          >
            From
          </label>
          <input
            id={fromId}
            type="date"
            value={value.from || ''}
            max={value.to || undefined}
            onChange={(event) => update('from', event.target.value)}
            className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-900 focus:border-indigo-500 focus:outline-none dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100"
          />
        </div>
        <div>
          <label
            htmlFor={toId}
            className="mb-1 block text-xs font-medium text-gray-600 dark:text-gray-400"
          >
            To
          </label>
          <input
            id={toId}
            type="date"
            value={value.to || ''}
            min={value.from || undefined}
            onChange={(event) => update('to', event.target.value)}
            className="w-full rounded-lg border border-gray-300 bg-white px-3 py-2 text-sm text-gray-900 focus:border-indigo-500 focus:outline-none dark:border-gray-700 dark:bg-gray-900 dark:text-gray-100"
          />
        </div>
      </div>
    </fieldset>
  );
}
