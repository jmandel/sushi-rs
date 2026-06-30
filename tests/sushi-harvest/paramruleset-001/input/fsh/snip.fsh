
        RuleSet: FirstRuleSet (system, strength)
        * code from http://example.org/{system}/info.html {strength}
        * pig from egg

        Profile: MyObservation
        Parent: Observation

        RuleSet: SecondRuleSet (min)
        * stuff {min}..

        Alias: $Something = http://example.org/Something

        RuleSet: ThirdRuleSet(cookie)
        * code from {cookie}

        Extension: MyExtension

        RuleSet: FourthRuleSet(toast)
        * reason ^short = {toast}

        Instance: ExampleObservation
        InstanceOf: MyObservation

        RuleSet: FifthRuleSet(strength, system)
        * code from {system} {strength}

        ValueSet: MyValueSet

        RuleSet: SixthRuleSet(content)
        * ^description = {content}

        Invariant: cat-1

        RuleSet: SeventhRuleSet(even, more)
        * content[+] = {even}
        * content[+] = {more}
        
        CodeSystem: MyCodeSystem

        RuleSet: EighthRuleSet(continuation)
        * continuation = {continuation} (exactly)

        Mapping: SomeMapping

        RuleSet: NinthRuleSet(tiring)
        * code from {tiring}

        Logical: MyLogical

        RuleSet: TenthRuleSet(conclusion)
        * valueString = {conclusion}

        Resource: MyResource
      