## New tool: `request_user_form_input`

The agent creates a form that the user inputs. This should be used when authoring web content and the user requests something like "Build this page, but let me author it". AI-written text is a problem in places where the human voice should come through. 

So, what if instead, the agent builds the layout, then shows a form to the user like this (sketch of tool call payload):

```json
{
  "title": "Landing page content",
  "description": "Author the content of the landing page and I'll insert it for you",
  "elements": [
    {
      "type": "row",
      "elements": [
        {
          "type": "input",
          "id": "mainTitle",
          "title": "Landing page title",
          "hoverDescription": "The title of the landing page should be blah blah..."
        },
        {
          "type": "input",
          "id": "quote",
          "title": "Daily quote",
        }
      ],
    },
    {
      "type": "input:markdown",
      "id": "mainContent",
      "title": "...",
      "hoverDescription": "..."
    },
  ]
}
```

Typically this is something the user should request. But it's something you as the agent might suggest when appropriate. Think through the scenarios you might encounter and structure the description you will see, so it brings forth some of the reflections you make now. If you're in doubt, it's also fine to keep this as something the user must request. Would be nice to have it as a slash command though.
