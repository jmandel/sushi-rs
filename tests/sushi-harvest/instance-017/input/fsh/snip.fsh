
        RuleSet: Question(context, linkId, text, type, repeats)
        * {context}item[+].linkId = "{linkId}"
        * {context}item[=].text = "{text}"
        * {context}item[=].type = #{type}
        * {context}item[=].repeats = {repeats}

        Instance: case-reporting-questionnaire
        InstanceOf: Questionnaire
        * insert Question(,title, HIV Case Report, display, false)
        