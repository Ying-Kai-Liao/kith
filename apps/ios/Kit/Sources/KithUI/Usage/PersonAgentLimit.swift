import SwiftUI

/// One person's monthly limit on the agent `spend` is for, with this month's spend. For the person page; nothing
/// shows for a non-owner or an older host. The stock `BudgetRow` does the pick and the custom amount.
public struct PersonAgentLimit: View {
    let spend: PeopleSpend
    let personId: String
    let name: String

    public init(spend: PeopleSpend, personId: String, name: String) {
        self.spend = spend
        self.personId = personId
        self.name = name
    }

    public var body: some View {
        if let people = spend.people {
            BudgetRow(name: name, line: people.line(for: personId), budget: people.spend(of: personId).budget, busy: spend.busy != nil) { b in
                Task { await spend.setBudget(personId, b) }
            }
        }
    }
}
