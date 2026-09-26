import { render } from '@testing-library/react';
import { axe, toHaveNoViolations } from 'jest-axe';
import EscrowCreationWizard from '@/components/escrow/EscrowCreationWizard';

expect.extend(toHaveNoViolations);

const axeRunner = global.axe || axe;

describe('Accessibility — EscrowCreationWizard', () => {
  it('has no violations on the initial parties step', async () => {
    const { container } = render(
      <EscrowCreationWizard onSuccess={jest.fn()} onCancel={jest.fn()} />,
    );

    expect(await axeRunner(container)).toHaveNoViolations();
  });
});
