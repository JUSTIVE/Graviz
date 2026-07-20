export const SAMPLE_SDL = `"""A user in the system."""
type User implements Node {
  """Globally unique identifier."""
  id: ID!
  """Display name shown across the app."""
  name: String!
  """Primary contact email, unique per account."""
  email: String!
  """Permission level for this user."""
  role: Role!
  """Every post the user has authored, newest first."""
  posts: [Post!]!
  """Optional public profile. Null until the user fills one in."""
  profile: Profile
}

"""Public-facing details a user chooses to share."""
type Profile {
  """Short self-introduction, markdown allowed."""
  bio: String
  """Absolute URL of the avatar image."""
  avatarUrl: String
  """The user this profile belongs to."""
  owner: User!
}

"""Anything that can be fetched by id."""
interface Node {
  """Globally unique identifier."""
  id: ID!
}

"""A piece of writing published on the platform."""
type Post implements Node {
  """Globally unique identifier."""
  id: ID!
  """Headline shown in lists and at the top of the article."""
  title: String!
  """Full article body, markdown source."""
  body: String!
  """The user who wrote this post."""
  author: User!
  """Topic labels used for discovery and filtering."""
  tags: [Tag!]!
  """Where the post sits in its publishing lifecycle."""
  status: PostStatus!
}

"""A topic label that can be attached to posts."""
type Tag {
  """Globally unique identifier."""
  id: ID!
  """Human-readable tag text, e.g. "graphql"."""
  label: String!
}

"""What a user is allowed to do."""
enum Role {
  """Full access, including user management."""
  ADMIN
  """Can create and edit any post."""
  EDITOR
  """Read-only access."""
  VIEWER
}

"""Publishing lifecycle of a post."""
enum PostStatus {
  """Being written; visible only to the author."""
  DRAFT
  """Live and visible to everyone."""
  PUBLISHED
  """Hidden from lists but kept for reference."""
  ARCHIVED
}

"""Any entity that can appear in site-wide search results."""
union SearchResult = User | Post | Tag

"""An ISO-8601 timestamp string, e.g. "2026-01-01T09:00:00Z"."""
scalar DateTime

"""Read entry points."""
type Query {
  """The currently authenticated user, or null when signed out."""
  me: User
  """Look a user up by id."""
  user(id: ID!): User
  """List posts, optionally filtered to one status."""
  posts(status: PostStatus): [Post!]!
  """Full-text search across users, posts, and tags."""
  search(term: String!): [SearchResult!]!
}

"""Write entry points."""
type Mutation {
  """Create a new draft post owned by the current user."""
  createPost(input: CreatePostInput!): Post!
  """Publish an existing draft, optionally on a schedule."""
  publishPost(post: PublishPostInput!, options: PublishOptionsInput!): PublishResult!
}

"""Everything that happened as a result of publishing."""
type PublishResult {
  """The post in its published state."""
  post: Post!
  """Followers who received a notification."""
  notifiedUsers: [User!]!
  """When the post will go live; null when published immediately."""
  scheduledAt: DateTime
}

"""Fields required to create a draft."""
input CreatePostInput {
  """Headline of the new post."""
  title: String!
  """Markdown body of the new post."""
  body: String!
  """Ids of tags to attach at creation time."""
  tags: [ID!]
}

"""Identifies the draft to publish and optional edits to apply."""
input PublishPostInput {
  """Id of the draft being published."""
  postId: ID!
  """Replacement headline, if editing while publishing."""
  title: String
  """Replacement body, if editing while publishing."""
  body: String
}

"""Knobs controlling how the publish happens."""
input PublishOptionsInput {
  """Whether to notify the author's followers."""
  notifyFollowers: Boolean!
  """Delay going live until this time; publish now when omitted."""
  scheduledAt: DateTime
  """Extra tags to attach as part of publishing."""
  tags: [ID!]
}
`;
